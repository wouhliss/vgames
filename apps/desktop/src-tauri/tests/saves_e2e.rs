//! PLAY-06 acceptance: cloud saves through the **real** API (in process, on a fresh PostgreSQL
//! database, fs storage). One player on two simulated devices covers every row of the pull
//! decision table, the three conflict choices that change data (Cancel changes nothing by
//! construction), an offline pull and push, a full quota and two devices committing on the same
//! head at once. A save-file kill during restore and the Proton-prefix layout have their own tests
//! next to the restore engine and `saves::locations`.
//!
//! Needs PostgreSQL 18 (or any version with `uuidv7()`):
//! `VGAMES_TEST_DATABASE_URL=postgres://vgames:vgames-dev-only@127.0.0.1:5432/postgres \
//!  cargo test -p vgames-desktop --test saves_e2e -- --ignored`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

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
use vgames_desktop_lib::saves::sync::{Pull, Push, Resolution, Sync};
use vgames_desktop_lib::saves::transaction::Transactions;
use vgames_desktop_lib::secrets::{MemoryVault, VaultHandle};

const SERVER_ID: &str = "01920000-0000-7000-8000-00000005a7e5";

// ---- the API ----------------------------------------------------------------------------------

struct Api {
    origin: String,
    pool: PgPool,
    _storage: tempfile::TempDir,
    _state: vgames_api::AppState,
}

async fn start_api(quota_bytes: Option<u64>) -> Api {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let admin_url = std::env::var("VGAMES_TEST_DATABASE_URL")
        .expect("set VGAMES_TEST_DATABASE_URL to a PostgreSQL maintenance database URL");
    let admin = PgPool::connect(&admin_url).await.unwrap();
    let db_name = format!("vgames_saves_{}", Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db_name}")))
        .execute(&admin)
        .await
        .unwrap();
    let mut url = url::Url::parse(&admin_url).unwrap();
    url.set_path(&db_name);
    let pool = PgPool::connect(url.as_str()).await.unwrap();
    vgames_api::db::MIGRATOR.run(&pool).await.unwrap();

    // Bound first: signed storage URLs carry the public origin, so it must be this port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let storage = tempfile::Builder::new()
        .prefix("vgames-saves-")
        .tempdir()
        .unwrap();
    let mut env: HashMap<&str, String> = HashMap::from([
        ("VGAMES_PUBLIC_URL", origin.clone()),
        ("VGAMES_SERVER_ID", SERVER_ID.to_owned()),
        ("DATABASE_URL", url.to_string()),
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
        (
            "VGAMES_FS_STORAGE_ROOT",
            storage.path().display().to_string(),
        ),
        (
            "VGAMES_FS_URL_SIGNING_KEY",
            "dGVzdC1mcy1zaWduaW5nLWtleS10ZXN0LWZzLXNpZ25pbmc=".to_owned(),
        ),
    ]);
    if let Some(quota) = quota_bytes {
        env.insert("VGAMES_SAVE_QUOTA_BYTES_PER_PACKAGE", quota.to_string());
    }
    let config = vgames_api::Config::from_lookup(|k| env.get(k).cloned()).unwrap();
    let state = vgames_api::AppState::new(config, pool.clone()).unwrap();
    tokio::spawn(vgames_api::server::serve_on(listener, state.clone()));
    Api {
        origin,
        pool,
        _storage: storage,
        _state: state,
    }
}

impl Api {
    /// A player and a published game: their id, the game's id and a desktop access token.
    async fn player_and_game(&self) -> (Uuid, Uuid, String) {
        let user: Uuid = sqlx::query_scalar(
            "INSERT INTO users (discord_id, username) VALUES ('700000000000000101', 'saver') RETURNING id",
        )
        .fetch_one(&self.pool)
        .await
        .unwrap();
        let package: Uuid = sqlx::query_scalar(
            "INSERT INTO packages (slug, title, status, created_by) VALUES ('saves-game', 'Saves Game', 'published', $1) RETURNING id",
        )
        .bind(user)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        (user, package, self.session(user).await)
    }

    async fn session(&self, user: Uuid) -> String {
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
             VALUES ($1, 'desktop', $2, now() + interval '1 day', $3, now() + interval '30 days')",
        )
        .bind(user)
        .bind(Sha256::digest(token.as_bytes()).to_vec())
        .bind(Sha256::digest(refresh).to_vec())
        .execute(&self.pool)
        .await
        .unwrap();
        token
    }

    async fn head(&self, user: Uuid, package: Uuid) -> Option<Uuid> {
        sqlx::query_scalar(
            "SELECT snapshot_id FROM save_heads WHERE user_id = $1 AND package_id = $2",
        )
        .bind(user)
        .bind(package)
        .fetch_optional(&self.pool)
        .await
        .unwrap()
    }
}

// ---- a device ----------------------------------------------------------------------------------

/// One computer of the player: its own launcher database, backups and save folder.
struct Device {
    root: tempfile::TempDir,
    scope: Scope,
    store: Store,
    sync: Sync,
}

impl Device {
    async fn new(origin: &str, user: Uuid, package: Uuid, token: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let server_id: Uuid = SERVER_ID.parse().unwrap();
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
                    "expires_in": 86_400,
                    "user": { "id": user, "username": "saver", "role": "user",
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
            store.clone(),
            &data,
        );
        std::fs::create_dir_all(root.path().join("docs/game")).unwrap();
        Self {
            root,
            scope: Scope {
                package: PackageRef {
                    server_id,
                    package_id: package,
                },
                user_id: user,
            },
            store,
            sync,
        }
    }

    fn folder(&self) -> PathBuf {
        self.root.path().join("docs/game")
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.folder().join(name), bytes).unwrap();
    }

    fn read(&self, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.folder().join(name)).ok()
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

    fn protected() -> Arc<ProtectedInstalls> {
        Arc::new(ProtectedInstalls::open(std::iter::empty()).unwrap())
    }

    async fn pull(&self) -> Pull {
        self.sync
            .pull(
                self.scope,
                self.locations(),
                Self::protected(),
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

    async fn pushed(&self) -> Uuid {
        match self.push().await {
            Push::Pushed { snapshot } => snapshot,
            other => panic!("expected a push, got {other:?}"),
        }
    }
}

fn conflict_head(pull: Pull) -> Uuid {
    match pull {
        Pull::Conflict { head } => head.summary.id,
        other => panic!("expected a conflict, got {other:?}"),
    }
}

// ---- the scenarios -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn two_devices_sync_through_every_decision_and_conflict_choice() {
    let api = start_api(None).await;
    let (user, package, token) = api.player_and_game().await;
    let a = Device::new(&api.origin, user, package, &token).await;
    let b = Device::new(&api.origin, user, package, &token).await;

    // No cloud saves yet: device A's progress is uploaded after its session.
    a.write("slot.sav", b"a1");
    assert!(matches!(a.pull().await, Pull::LocalProgress));
    let s1 = a.pushed().await;
    assert_eq!(api.head(user, package).await, Some(s1));

    // A fresh device gets the cloud saves before its first launch, then has nothing to do.
    assert!(matches!(b.pull().await, Pull::Restored { snapshot } if snapshot == s1));
    assert_eq!(b.read("slot.sav").unwrap(), b"a1");
    assert!(matches!(b.pull().await, Pull::Unchanged));
    assert_eq!(b.push().await, Push::Unchanged);

    // Only the cloud moved: restored over unchanged local saves.
    a.write("slot.sav", b"a2");
    let s2 = a.pushed().await;
    assert!(matches!(b.pull().await, Pull::Restored { snapshot } if snapshot == s2));
    assert_eq!(b.read("slot.sav").unwrap(), b"a2");

    // Only this device moved: pushed with the head as parent.
    b.write("slot.sav", b"b2");
    assert!(matches!(b.pull().await, Pull::LocalProgress));
    let s3 = b.pushed().await;
    assert!(matches!(a.pull().await, Pull::Restored { snapshot } if snapshot == s3));

    // Both moved: B's commit on an old parent is refused, and its pull reports the conflict.
    a.write("slot.sav", b"a3");
    let s4 = a.pushed().await;
    b.write("slot.sav", b"b3");
    assert_eq!(b.push().await, Push::Conflict);
    assert_eq!(conflict_head(b.pull().await), s4);
    // Keep cloud: B gets A's save; its own was backed up first.
    let backups = b.store.backups(b.scope).await.unwrap().len();
    let kept = b
        .sync
        .keep_cloud(
            b.scope,
            b.locations(),
            Device::protected(),
            s4,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(kept, Resolution::Done));
    assert_eq!(b.read("slot.sav").unwrap(), b"a3");
    assert!(b.store.backups(b.scope).await.unwrap().len() > backups);
    assert!(matches!(b.pull().await, Pull::Unchanged));

    // Keep this device: B's save becomes the head on top of A's.
    a.write("slot.sav", b"a4");
    let s5 = a.pushed().await;
    b.write("slot.sav", b"b4");
    assert_eq!(conflict_head(b.pull().await), s5);
    let kept = b
        .sync
        .keep_device(b.scope, b.locations(), s5, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(kept, Resolution::Done));
    let head = api.head(user, package).await.unwrap();
    assert_ne!(head, s5);
    assert!(matches!(a.pull().await, Pull::Restored { snapshot } if snapshot == head));
    assert_eq!(a.read("slot.sav").unwrap(), b"b4");

    // Keep both: A keeps the cloud side in a local backup, then its own save goes up.
    b.write("slot.sav", b"b5");
    let s6 = b.pushed().await;
    a.write("slot.sav", b"a5");
    assert_eq!(conflict_head(a.pull().await), s6);
    let backups = a.store.backups(a.scope).await.unwrap().len();
    let kept = a
        .sync
        .keep_both(
            a.scope,
            a.locations(),
            Device::protected(),
            s6,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(kept, Resolution::Done));
    assert_eq!(a.read("slot.sav").unwrap(), b"a5");
    assert_eq!(a.store.backups(a.scope).await.unwrap().len(), backups + 1);
    let head = api.head(user, package).await.unwrap();
    assert!(matches!(b.pull().await, Pull::Restored { snapshot } if snapshot == head));
    assert_eq!(b.read("slot.sav").unwrap(), b"a5");

    // A choice made against a head that moved meanwhile renews the conflict instead.
    b.write("slot.sav", b"b6");
    let s7 = b.pushed().await;
    a.write("slot.sav", b"a6");
    assert_eq!(conflict_head(a.pull().await), s7);
    b.write("slot.sav", b"b7");
    let s8 = b.pushed().await;
    let renewed = a
        .sync
        .keep_cloud(
            a.scope,
            a.locations(),
            Device::protected(),
            s7,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(renewed, Resolution::Renewed(head) if head.id == s8));
    assert_eq!(a.read("slot.sav").unwrap(), b"a6");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn two_devices_committing_on_one_head_at_once_leave_one_head_and_one_conflict() {
    let api = start_api(None).await;
    let (user, package, token) = api.player_and_game().await;
    let a = Device::new(&api.origin, user, package, &token).await;
    let b = Device::new(&api.origin, user, package, &token).await;
    a.write("slot.sav", b"start");
    a.pushed().await;
    assert!(matches!(b.pull().await, Pull::Restored { .. }));
    for round in 0..5 {
        a.write("slot.sav", format!("a{round}").as_bytes());
        b.write("slot.sav", format!("b{round}").as_bytes());
        let (from_a, from_b) = tokio::join!(a.push(), b.push());
        let outcomes = [from_a, from_b];
        let pushed: Vec<_> = outcomes
            .iter()
            .filter_map(|o| match o {
                Push::Pushed { snapshot } => Some(*snapshot),
                _ => None,
            })
            .collect();
        assert_eq!(pushed.len(), 1, "round {round}: {outcomes:?}");
        assert!(
            outcomes.contains(&Push::Conflict),
            "round {round}: {outcomes:?}"
        );
        assert_eq!(api.head(user, package).await, Some(pushed[0]));
        // The loser takes the winner's save to start the next round from one head.
        let (winner, loser) = if matches!(outcomes[0], Push::Pushed { .. }) {
            (&a, &b)
        } else {
            (&b, &a)
        };
        let head = conflict_head(loser.pull().await);
        loser
            .sync
            .keep_cloud(
                loser.scope,
                loser.locations(),
                Device::protected(),
                head,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(loser.read("slot.sav"), winner.read("slot.sav"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn offline_and_a_full_quota_keep_local_saves_pending() {
    let api = start_api(Some(1024 * 1024)).await;
    let (user, package, token) = api.player_and_game().await;

    // A full quota: nothing is uploaded, local saves stay, sync stays pending.
    let device = Device::new(&api.origin, user, package, &token).await;
    let big = vec![7u8; 2 * 1024 * 1024];
    device.write("slot.sav", &big);
    assert_eq!(device.push().await, Push::QuotaExceeded);
    assert_eq!(device.read("slot.sav").unwrap(), big);
    assert!(device.store.state(device.scope).await.unwrap().pending);
    assert_eq!(api.head(user, package).await, None);

    // Unreachable: the game starts with local saves and both directions wait.
    let offline = Device::new("http://127.0.0.1:1", user, package, &token).await;
    offline.write("slot.sav", b"local");
    assert!(matches!(offline.pull().await, Pull::Offline));
    assert_eq!(offline.push().await, Push::Offline);
    assert_eq!(offline.read("slot.sav").unwrap(), b"local");
    assert!(offline.store.state(offline.scope).await.unwrap().pending);
}
