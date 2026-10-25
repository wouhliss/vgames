//! PLAY-06 acceptance: one player's cloud saves travel Windows → Linux → macOS. Each hosted runner
//! runs one step against the real API (in process) on server state the previous runner left
//! behind, moved as a workflow artifact: the rows of every table cloud saves use (exported as
//! JSON, so each runner can use whatever PostgreSQL it has) and the fs storage folder. A fresh
//! device pulls, checks every file and its Windows-relative snapshot path, changes the saves and
//! pushes. `.github/workflows/saves-roundtrip.yml` runs the steps and carries the folder.
//!
//! Environment:
//! - `VGAMES_TEST_DATABASE_URL`: an empty database for this step (migrated here, then loaded
//!   with the previous step's rows).
//! - `VGAMES_ROUNDTRIP_DIR`: the carried state (`storage/`, `server.json`, `state.json`).
//! - `VGAMES_ROUNDTRIP_STEP`: `1`, `2` or `3`.
//!
//! `cargo test -p vgames-desktop --test saves_roundtrip -- --ignored`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_core::manifest::{SaveBase, SaveLocation};
use vgames_desktop_lib::api::{ApiClient, Session};
use vgames_desktop_lib::db::Db;
use vgames_desktop_lib::db::saves::{Scope, Store};
use vgames_desktop_lib::events::PackageRef;
use vgames_desktop_lib::saves::blobs::Blobs;
use vgames_desktop_lib::saves::http;
use vgames_desktop_lib::saves::policy::ProtectedInstalls;
use vgames_desktop_lib::saves::scan::Location;
use vgames_desktop_lib::saves::sync::{Pull, Push, Sync};
use vgames_desktop_lib::saves::transaction::Transactions;
use vgames_desktop_lib::secrets::{MemoryVault, VaultHandle};

const SERVER_ID: &str = "01920000-0000-7000-8000-00000005a7e6";
const USER_ID: &str = "01920000-0000-7000-8000-0000000a11ce";
const PACKAGE_ID: &str = "01920000-0000-7000-8000-00000000ca7e";

/// What the previous step left for the next one.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// The cloud head the last step pushed.
    head: Option<Uuid>,
    /// Save-relative path (`/`-separated) → contents, as the last step pushed them.
    files: BTreeMap<String, String>,
    /// The OS of each step so far.
    steps: Vec<String>,
}

fn os() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "one step of the hosted cross-OS round trip: see saves-roundtrip.yml"]
async fn cloud_saves_round_trip_step() {
    let url = std::env::var("VGAMES_TEST_DATABASE_URL").expect("VGAMES_TEST_DATABASE_URL");
    let dir = PathBuf::from(std::env::var("VGAMES_ROUNDTRIP_DIR").expect("VGAMES_ROUNDTRIP_DIR"));
    let step: u32 = std::env::var("VGAMES_ROUNDTRIP_STEP")
        .expect("VGAMES_ROUNDTRIP_STEP")
        .parse()
        .unwrap();
    let pool = PgPool::connect(&url).await.unwrap();
    let state_file = dir.join("state.json");
    vgames_api::db::MIGRATOR.run(&pool).await.unwrap();
    let server_file = dir.join("server.json");
    let mut state: State = if step == 1 {
        seed(&pool).await;
        State::default()
    } else {
        import(&pool, &std::fs::read(&server_file).unwrap()).await;
        serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap()
    };
    assert_eq!(
        state.steps.len() + 1,
        step as usize,
        "steps so far: {:?}",
        state.steps
    );

    let origin = serve(&url, &dir.join("storage")).await;
    let token = session(&pool).await;
    let device = Device::new(&origin, &token).await;

    // A fresh device gets exactly what the previous OS pushed, under the same names.
    if let Some(head) = state.head {
        let pulled = device.pull().await;
        assert!(
            matches!(pulled, Pull::Restored { snapshot } if snapshot == head),
            "step {step} on {}: {pulled:?}",
            os()
        );
        assert_eq!(device.files(), state.files, "step {step} on {}", os());
    } else {
        assert!(matches!(device.pull().await, Pull::Unchanged));
    }

    // Change the saves: one file rewritten, one added in a nested folder with a space.
    let name = os();
    device.write("settings.ini", &format!("[game]\nlast_os={name}\n"));
    device.write(
        &format!("Profiles/Slot {step}/save.dat"),
        &format!("progress from {name}, step {step}"),
    );
    let head = match device.push().await {
        Push::Pushed { snapshot } => snapshot,
        other => panic!("step {step} on {name}: {other:?}"),
    };
    state.head = Some(head);
    state.files = device.files();
    state.steps.push(name.to_owned());
    std::fs::write(&state_file, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    std::fs::write(&server_file, export(&pool).await).unwrap();

    // The server stored Windows-relative, `/`-separated names on every OS.
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT files FROM save_snapshots WHERE id = $1")
            .bind(head)
            .fetch_one(&pool)
            .await
            .unwrap();
    let mut paths: Vec<String> = stored
        .as_array()
        .unwrap()
        .iter()
        .map(|file| {
            format!(
                "{}/{}",
                file["root"].as_str().unwrap(),
                file["path"].as_str().unwrap()
            )
        })
        .collect();
    paths.sort();
    let mut expected: Vec<String> = state
        .files
        .keys()
        .map(|path| format!("game/{path}"))
        .collect();
    expected.sort();
    assert_eq!(paths, expected);
    assert!(paths.iter().all(|path| !path.contains('\\')));
}

/// The tables cloud saves read and write, parents first; rows in id order so a snapshot's parent
/// (UUIDv7, older) is loaded before it.
const TABLES: &[(&str, &str)] = &[
    ("users", "id"),
    ("packages", "id"),
    ("save_blobs", "user_id, blake3"),
    ("save_snapshots", "id"),
    ("save_snapshot_blobs", "snapshot_id, blake3"),
    ("save_heads", "user_id, package_id"),
];

/// Every row of `TABLES` as JSON: independent of the PostgreSQL version on either side.
async fn export(pool: &PgPool) -> Vec<u8> {
    let mut out = serde_json::Map::new();
    for (table, order) in TABLES {
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT to_jsonb(t) FROM {table} t ORDER BY {order}"
        )))
        .fetch_all(pool)
        .await
        .unwrap();
        out.insert((*table).to_owned(), serde_json::Value::Array(rows));
    }
    serde_json::to_vec(&out).unwrap()
}

async fn import(pool: &PgPool, json: &[u8]) {
    let tables: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(json).unwrap();
    for (table, _) in TABLES {
        // Generated columns are computed again on insert.
        let columns: Vec<String> = sqlx::query_scalar(
            "SELECT quote_ident(column_name) FROM information_schema.columns
             WHERE table_schema = 'public' AND table_name = $1 AND is_generated = 'NEVER'
             ORDER BY ordinal_position",
        )
        .bind(*table)
        .fetch_all(pool)
        .await
        .unwrap();
        let columns = columns.join(", ");
        for row in tables[*table].as_array().unwrap() {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO {table} ({columns}) SELECT {columns} FROM jsonb_populate_record(NULL::{table}, $1)"
            )))
            .bind(row)
            .execute(pool)
            .await
            .unwrap();
        }
    }
}

async fn seed(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO users (id, discord_id, username) VALUES ($1, '700000000000000201', 'traveller')",
    )
    .bind(Uuid::parse_str(USER_ID).unwrap())
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO packages (id, slug, title, status, created_by) VALUES ($1, 'round-trip', 'Round Trip', 'published', $2)",
    )
    .bind(Uuid::parse_str(PACKAGE_ID).unwrap())
    .bind(Uuid::parse_str(USER_ID).unwrap())
    .execute(pool)
    .await
    .unwrap();
}

/// A desktop session for this step's device: its access token.
async fn session(pool: &PgPool) -> String {
    use sha2::{Digest, Sha256};
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).unwrap();
    let token = format!(
        "vga_{}",
        base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, raw)
    );
    let mut refresh = [0u8; 32];
    getrandom::fill(&mut refresh).unwrap();
    sqlx::query(
        "INSERT INTO sessions (user_id, kind, access_token_hash, access_expires_at, refresh_token_hash, refresh_expires_at)
         VALUES ($1, 'desktop', $2, now() + interval '1 hour', $3, now() + interval '1 day')",
    )
    .bind(Uuid::parse_str(USER_ID).unwrap())
    .bind(Sha256::digest(token.as_bytes()).to_vec())
    .bind(Sha256::digest(refresh).to_vec())
    .execute(pool)
    .await
    .unwrap();
    token
}

/// The API on `url` with fs storage under `storage`; returns its origin.
async fn serve(url: &str, storage: &Path) -> String {
    std::fs::create_dir_all(storage).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let env: HashMap<&str, String> = HashMap::from([
        ("VGAMES_PUBLIC_URL", origin.clone()),
        ("VGAMES_SERVER_ID", SERVER_ID.to_owned()),
        ("DATABASE_URL", url.to_owned()),
        (
            "VGAMES_ROOT_PUBLIC_KEY",
            "sn/b0D2mU+3RfBglb8/Jy/2BfTWHvE2SyIgXkx3m8U8=".to_owned(),
        ),
        (
            "VGAMES_SERVER_SECRET",
            "dGVzdC1zZXJ2ZXItc2VjcmV0LXRlc3Qtc2VydmVyLXNlY3JldA==".to_owned(),
        ),
        ("VGAMES_DEV_FAKE_DISCORD", "true".to_owned()),
        ("VGAMES_STORAGE_BACKEND", "fs".to_owned()),
        ("VGAMES_FS_STORAGE_ROOT", storage.display().to_string()),
        (
            "VGAMES_FS_URL_SIGNING_KEY",
            "dGVzdC1mcy1zaWduaW5nLWtleS10ZXN0LWZzLXNpZ25pbmc=".to_owned(),
        ),
    ]);
    let config = vgames_api::Config::from_lookup(|k| env.get(k).cloned()).unwrap();
    let pool = PgPool::connect(url).await.unwrap();
    let state = vgames_api::AppState::new(config, pool).unwrap();
    tokio::spawn(vgames_api::server::serve_on(listener, state));
    origin
}

/// A freshly installed launcher on this OS.
struct Device {
    root: tempfile::TempDir,
    scope: Scope,
    sync: Sync,
}

impl Device {
    async fn new(origin: &str, token: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let server_id: Uuid = SERVER_ID.parse().unwrap();
        let user: Uuid = USER_ID.parse().unwrap();
        let db = Db::open(&root.path().join("launcher.sqlite3")).unwrap();
        db.call(move |conn| {
            conn.execute(
                "INSERT INTO servers (id,url,name,root_public_key,root_fingerprint,added_at) VALUES (?1,'https://saves.example.invalid','Saves',zeroblob(32),'VG1',0)",
                [server_id.to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let store = Store::new(db);
        let base = url::Url::parse(&format!("{origin}/")).unwrap();
        let session = Arc::new(Session::new(
            server_id,
            base.clone(),
            VaultHandle(Arc::new(MemoryVault::default())),
            Arc::new(|_| {}),
        ));
        session
            .store(
                &serde_json::from_value(json!({
                    "access_token": token, "refresh_token": "unused", "token_type": "Bearer",
                    "expires_in": 3600,
                    "user": { "id": user, "username": "traveller", "role": "user",
                              "created_at": "2026-10-25T12:00:00Z" }
                }))
                .unwrap(),
            )
            .await;
        let http = http::Client::new(
            ApiClient::new(
                vgames_desktop_lib::api::http_client().unwrap(),
                base,
                session,
            ),
            CancellationToken::new(),
        );
        let data = root.path().join("data");
        let sync = Sync::new(
            http,
            Blobs::new(true).unwrap(),
            Transactions::new(store.clone(), &data),
            store,
            &data,
        );
        std::fs::create_dir_all(root.path().join("docs/game")).unwrap();
        Self {
            root,
            scope: Scope {
                package: PackageRef {
                    server_id,
                    package_id: PACKAGE_ID.parse().unwrap(),
                },
                user_id: user,
            },
            sync,
        }
    }

    fn folder(&self) -> PathBuf {
        self.root.path().join("docs").join("game")
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = relative
            .split('/')
            .fold(self.folder(), |path, part| path.join(part));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// Every save file, keyed by its `/`-separated path under the save folder.
    fn files(&self) -> BTreeMap<String, String> {
        fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name().into_string().unwrap();
                let key = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                if entry.file_type().unwrap().is_dir() {
                    walk(&entry.path(), &key, out);
                } else {
                    out.insert(key, std::fs::read_to_string(entry.path()).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.folder(), "", &mut out);
        out
    }

    fn locations(&self) -> Arc<Vec<Location>> {
        let config = SaveLocation {
            id: "game".into(),
            base: SaveBase::Documents,
            path: "game".into(),
            include: vec![],
            exclude: vec![],
        };
        Arc::new(vec![
            Location::open(&config, &self.root.path().join("docs")).unwrap(),
        ])
    }

    async fn pull(&self) -> Pull {
        self.sync
            .pull(
                self.scope,
                self.locations(),
                Arc::new(ProtectedInstalls::open(std::iter::empty()).unwrap()),
                CancellationToken::new(),
            )
            .await
            .unwrap()
    }

    async fn push(&self) -> Push {
        self.sync
            .push(self.scope, self.locations(), CancellationToken::new())
            .await
            .unwrap()
    }
}
