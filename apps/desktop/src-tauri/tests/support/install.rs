//! The real API in process, publishing through `vgames_transfer::upload::publish`, and a
//! launcher profile on disk (INS-08). Used by `tests/install_e2e.rs`.

use std::collections::HashMap;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::PgPool;
use uuid::Uuid;
use vgames_core::trust::{FORMAT, PublisherKey, TrustBundle, sign_bundle};
use vgames_desktop_lib::db::Db;
use vgames_desktop_lib::downloads::model::DownloadSettings;
use vgames_desktop_lib::events::EventBus;
use vgames_desktop_lib::paths::AppPaths;
use vgames_desktop_lib::secrets::{MemoryVault, TokenVault, VaultHandle};
use vgames_desktop_lib::servers::{BrowserOpener, Servers, ServersConfig};
use vgames_desktop_lib::state::AppState;
use vgames_pack::manifest::{Execution, Launch, LaunchTarget};
use vgames_pack::scan::{self, FsReader};
use vgames_pack::{PACK_SIZE, PackSource, Plan};
use vgames_proto::versions::{FinalizeRequest, UploadTarget, Version, VersionCreate};
use vgames_transfer::download::RemoteError;
use vgames_transfer::testkit::package::{publisher_key, root_key};
use vgames_transfer::upload::UploadControl;
use vgames_transfer::upload::publish::{
    self, PublishApi, PublishControl, PublishOptions, PublishRequest,
};

const SERVER_ID: &str = "01920000-0000-7000-8000-0000000e2e01";
/// The dummy game's launch target (`launch.default`).
pub const GAME_SCRIPT: &str = "bin/game.sh";

// ---- the API ----------------------------------------------------------------------------------

pub struct Api {
    pub url: String,
    pub server_id: Uuid,
    /// An owner's access token; the owner also holds the publisher key.
    pub token: String,
    pool: PgPool,
    admin: PgPool,
    db_name: String,
    /// Removed on drop, also when the test fails.
    storage: tempfile::TempDir,
    state: vgames_api::AppState,
    http: reqwest::Client,
}

/// A published version.
pub struct Published {
    pub package_id: Uuid,
    pub version_id: Uuid,
}

impl Api {
    pub async fn start() -> Self {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
        let admin_url = std::env::var("VGAMES_TEST_DATABASE_URL")
            .expect("set VGAMES_TEST_DATABASE_URL to a PostgreSQL maintenance database URL");
        let admin = PgPool::connect(&admin_url).await.unwrap();
        let db_name = format!("vgames_e2e_{}", Uuid::now_v7().simple());
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
        let addr = listener.local_addr().unwrap();
        let origin = format!("http://127.0.0.1:{}", addr.port());
        let storage = tempfile::Builder::new()
            .prefix("vgames-e2e-")
            .tempdir()
            .unwrap();
        let env: HashMap<&str, String> = HashMap::from([
            ("VGAMES_PUBLIC_URL", origin.clone()),
            ("VGAMES_SERVER_ID", SERVER_ID.to_owned()),
            ("DATABASE_URL", url.to_string()),
            (
                "VGAMES_ROOT_PUBLIC_KEY",
                root_key().public_key().to_base64(),
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
        let config = vgames_api::Config::from_lookup(|k| env.get(k).cloned()).unwrap();
        let state = vgames_api::AppState::new(config, pool.clone()).unwrap();
        tokio::spawn(vgames_api::server::serve_on(listener, state.clone()));
        // The server-side `version.verify` job runs on these workers.
        tokio::spawn(vgames_api::jobs::run_workers(state.clone()));

        let (owner_id, token) = seed_owner(&pool).await;
        let api = Self {
            url: origin,
            server_id: SERVER_ID.parse().unwrap(),
            token,
            pool,
            admin,
            db_name,
            storage,
            state,
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
        };
        api.upload_trust_bundle(owner_id).await;
        api
    }

    async fn upload_trust_bundle(&self, owner_id: Uuid) {
        let ts = |s: &str| s.parse().unwrap();
        let key = publisher_key();
        let bundle = TrustBundle {
            format: FORMAT.into(),
            server_id: self.server_id,
            version: 1,
            issued_at: ts("2026-01-01T00:00:00Z"),
            expires_at: None,
            root_key_id: root_key().public_key().key_id(),
            publishers: vec![PublisherKey {
                key_id: key.public_key().key_id(),
                public_key: key.public_key(),
                holder_user_id: owner_id,
                label: "e2e".into(),
                not_before: ts("2026-01-01T00:00:00Z"),
                not_after: ts("2099-01-01T00:00:00Z"),
            }],
            revoked: vec![],
            next_root: None,
        };
        let bytes = bundle.to_bytes();
        let body = json!({
            "bundle": STANDARD.encode(&bytes),
            "signature": sign_bundle(&root_key(), &bytes).to_base64(),
        });
        let resp = self
            .request(reqwest::Method::POST, "/v1/admin/trust/bundles")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(&self.token)
    }

    /// A new package, published from `source` (launch target `bin/game.sh` when present) and
    /// made visible to players.
    pub async fn publish_package(&self, title: &str, source: &Path) -> Published {
        let created: Value = self
            .request(reqwest::Method::POST, "/v1/admin/packages")
            .json(&json!({ "title": title, "fetch_metadata": false }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let package_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();

        let scanned = scan::scan(source).unwrap();
        assert!(
            scanned.rejected.is_empty(),
            "unpackable files in the source"
        );
        let plan = Plan::new(scanned.files, scanned.directories).unwrap();
        let packing = plan.raw_packing(PACK_SIZE).unwrap();
        let execution = if source.join(GAME_SCRIPT).is_file() {
            Execution {
                launch: Some(Launch {
                    default: "game".into(),
                    targets: vec![LaunchTarget {
                        id: "game".into(),
                        label: "Play".into(),
                        executable: GAME_SCRIPT.into(),
                        args: vec![],
                        working_dir: None,
                        env: Default::default(),
                    }],
                }),
                ..Execution::default()
            }
        } else {
            Execution::default()
        };
        let source = PackSource::new(
            Arc::new(plan),
            Arc::new(packing),
            Arc::new(FsReader::new(source)),
        );
        let create: VersionCreate = serde_json::from_value(json!({
            "platform": "linux-x86_64",
            "version_label": "1.0",
        }))
        .unwrap();
        let api = Arc::new(PublishClient {
            http: self.http.clone(),
            url: self.url.clone(),
            token: self.token.clone(),
        });
        let options = PublishOptions::default();
        let control = PublishControl::new(UploadControl::new());
        let version = publish::create_version(
            api.as_ref(),
            self.server_id,
            package_id,
            create.clone(),
            Uuid::now_v7(),
            &options,
            &control,
        )
        .await
        .unwrap();
        let resume = tempfile::tempdir().unwrap();
        let request = PublishRequest {
            version,
            server_id: self.server_id,
            package_id,
            version_create: create,
            execution,
            source,
            resume_path: resume.path().join("packs.json"),
            signing_key: Some(publisher_key()),
        };
        let version = publish::run(request, api, &options, &control)
            .await
            .unwrap();

        // Visible to players: `published` (JSON merge patch, with the current ETag).
        let current = self
            .request(
                reqwest::Method::GET,
                &format!("/v1/admin/packages/{package_id}"),
            )
            .send()
            .await
            .unwrap();
        let etag = current.headers()["etag"].to_str().unwrap().to_owned();
        let resp = self
            .request(
                reqwest::Method::PATCH,
                &format!("/v1/admin/packages/{package_id}"),
            )
            .header("if-match", etag)
            .header("content-type", "application/merge-patch+json")
            .body(json!({ "status": "published" }).to_string())
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success(), "{}", resp.text().await.unwrap());
        Published {
            package_id,
            version_id: version.id,
        }
    }

    /// Where the fs storage backend keeps an object of the `packages` bucket.
    fn object(&self, key: &str) -> PathBuf {
        self.storage.path().join("packages").join(key)
    }

    /// The published manifest, as stored.
    pub fn manifest(&self, published: &Published) -> vgames_core::manifest::Manifest {
        let key = format!(
            "v1/{}/{}/manifest.json",
            published.package_id, published.version_id
        );
        let bytes = std::fs::read(self.object(&key)).unwrap();
        vgames_core::manifest::parse_and_validate(&bytes).unwrap()
    }

    /// Flips one byte of a stored pack.
    pub fn flip_pack_byte(&self, published: &Published, pack: u32, offset: u64) {
        let key = format!(
            "v1/{}/{}/packs/{pack:05}.pack",
            published.package_id, published.version_id
        );
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.object(&key))
            .unwrap();
        let mut byte = [0u8; 1];
        file.seek(SeekFrom::Start(offset)).unwrap();
        std::io::Read::read_exact(&mut file, &mut byte).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&[byte[0] ^ 0xff]).unwrap();
    }

    pub async fn stop(self) {
        self.state.shutdown.cancel();
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(self.state);
        self.pool.close().await;
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            self.db_name
        )))
        .execute(&self.admin)
        .await;
    }
}

/// An owner and a long-lived desktop session (the nightly run takes a while).
async fn seed_owner(pool: &PgPool) -> (Uuid, String) {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).unwrap();
    let token = format!(
        "vga_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
    );
    let user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (discord_id, username, role) VALUES ('100000000000000777', 'e2e-owner', 'owner') RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let mut refresh = [0u8; 32];
    getrandom::fill(&mut refresh).unwrap();
    sqlx::query(
        "INSERT INTO sessions (user_id, kind, access_token_hash, access_expires_at, refresh_token_hash, refresh_expires_at)
         VALUES ($1, 'desktop', $2, now() + interval '12 hours', $3, now() + interval '30 days')",
    )
    .bind(user_id)
    .bind(Sha256::digest(token.as_bytes()).to_vec())
    .bind(Sha256::digest(refresh).to_vec())
    .execute(pool)
    .await
    .unwrap();
    (user_id, token)
}

/// `PublishApi` over HTTP, the same routes as the `vgames` CLI.
struct PublishClient {
    http: reqwest::Client,
    url: String,
    token: String,
}

impl PublishClient {
    async fn call<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        idempotency: Option<Uuid>,
    ) -> Result<T, RemoteError> {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(&self.token);
        if let Some(key) = idempotency {
            req = req.header("idempotency-key", key.to_string());
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.map_err(|e| RemoteError {
            retryable: true,
            code: None,
            message: e.to_string(),
        })?;
        let status = resp.status();
        if status.is_success() {
            return resp.json().await.map_err(|e| RemoteError {
                retryable: false,
                code: None,
                message: e.to_string(),
            });
        }
        let problem: Value = resp.json().await.unwrap_or(Value::Null);
        Err(RemoteError {
            retryable: status.as_u16() == 429 || status.is_server_error(),
            code: problem["code"].as_str().map(str::to_owned),
            message: format!("{status}: {problem}"),
        })
    }
}

impl PublishApi for PublishClient {
    async fn create_version(
        &self,
        package_id: Uuid,
        request: VersionCreate,
        idempotency_key: Uuid,
    ) -> Result<Version, RemoteError> {
        let path = format!("/v1/admin/packages/{package_id}/versions");
        let body = serde_json::to_value(request).unwrap();
        self.call(
            reqwest::Method::POST,
            &path,
            Some(body),
            Some(idempotency_key),
        )
        .await
    }

    async fn get_version(&self, version_id: Uuid) -> Result<Version, RemoteError> {
        let path = format!("/v1/admin/versions/{version_id}");
        self.call(reqwest::Method::GET, &path, None, None).await
    }

    async fn pack_upload_target(
        &self,
        version_id: Uuid,
        pack: u32,
    ) -> Result<UploadTarget, RemoteError> {
        let path = format!("/v1/admin/versions/{version_id}/packs/{pack}/upload-session");
        self.call(reqwest::Method::POST, &path, None, None).await
    }

    async fn manifest_upload_target(&self, version_id: Uuid) -> Result<UploadTarget, RemoteError> {
        let path = format!("/v1/admin/versions/{version_id}/manifest-upload");
        self.call(reqwest::Method::POST, &path, None, None).await
    }

    async fn finalize(
        &self,
        version_id: Uuid,
        request: FinalizeRequest,
    ) -> Result<Version, RemoteError> {
        let path = format!("/v1/admin/versions/{version_id}/finalize");
        let body = serde_json::to_value(request).unwrap();
        self.call(reqwest::Method::POST, &path, Some(body), None)
            .await
    }

    async fn publish(&self, version_id: Uuid) -> Result<Version, RemoteError> {
        let path = format!("/v1/admin/versions/{version_id}/publish");
        self.call(reqwest::Method::POST, &path, None, None).await
    }
}

// ---- the synthetic package ----------------------------------------------------------------------

/// Many small files, a few large ones and a launch script.
pub struct PackageSpec {
    small_files: usize,
    large_files: Vec<u64>,
}

const MIB: u64 = 1024 * 1024;

impl PackageSpec {
    /// 100,000 small files and 8 GiB of large ones at 1.0.
    pub fn scaled(scale: f64) -> Self {
        let small_files = ((100_000.0 * scale) as usize).max(200);
        let large_total = ((8.0 * 1024.0 * MIB as f64 * scale) as u64).max(24 * MIB);
        Self {
            small_files,
            large_files: vec![
                large_total / 2,
                large_total / 3,
                large_total - large_total / 2 - large_total / 3,
            ],
        }
    }

    /// For the flipped-byte check: one pack, stored chunks.
    pub fn small() -> Self {
        Self {
            small_files: 20,
            large_files: vec![3 * MIB],
        }
    }

    pub fn file_count(&self) -> usize {
        self.small_files + self.large_files.len() + 1
    }

    /// Writes the tree; returns its total size in bytes. Content is pseudo-random, so packs
    /// are stored rather than compressed.
    pub fn generate(&self, root: &Path) -> u64 {
        let mut rng = 0x9e37_79b9_7f4a_7c15u64 ^ self.small_files as u64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mut total = 0;
        for i in 0..self.small_files {
            let dir = root.join(format!("assets/{:03}", i / 1000));
            std::fs::create_dir_all(&dir).unwrap();
            let len = 100 + (next() % 1900) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            std::fs::write(dir.join(format!("{i:06}.bin")), &bytes).unwrap();
            total += len as u64;
        }
        std::fs::create_dir_all(root.join("data")).unwrap();
        for (i, size) in self.large_files.iter().enumerate() {
            let mut file = std::io::BufWriter::new(
                std::fs::File::create(root.join(format!("data/large{i}.bin"))).unwrap(),
            );
            let mut left = *size;
            let mut block = vec![0u8; 1 << 20];
            while left > 0 {
                for word in block.as_chunks_mut::<8>().0 {
                    *word = next().to_le_bytes();
                }
                let n = left.min(block.len() as u64) as usize;
                file.write_all(&block[..n]).unwrap();
                left -= n as u64;
            }
            file.flush().unwrap();
            total += size;
        }
        // The dummy game: records that it ran, next to itself.
        let script = root.join(GAME_SCRIPT);
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        let body = b"#!/bin/sh\necho launched > \"$(dirname \"$0\")/launched.txt\"\n";
        std::fs::write(&script, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        total + body.len() as u64
    }
}

// ---- the launcher -------------------------------------------------------------------------------

/// A launcher profile on disk; a second `open` of the same folder is a restarted launcher.
pub struct Desktop {
    pub state: AppState,
}

struct NoBrowser;

impl BrowserOpener for NoBrowser {
    fn open(&self, _url: &url::Url) -> bool {
        false
    }
}

impl Desktop {
    pub async fn open(data_dir: &Path, server_id: Uuid, token: &str) -> Self {
        std::fs::create_dir_all(data_dir).unwrap();
        let db = Db::open(&data_dir.join("launcher.db")).unwrap();
        let bus = EventBus::new();
        // Signed in: the session the API seeded (the keychain is per process).
        let vault = Arc::new(MemoryVault::default());
        let tokens = json!({ "access": token, "refresh": "vgr_e2e-not-used" });
        vault
            .set(&format!("tokens:{server_id}"), &tokens.to_string())
            .unwrap();
        let config = ServersConfig {
            allow_loopback_http: true,
            launcher_version: semver::Version::new(1, 0, 0),
            device_name: "e2e".into(),
            browser: Arc::new(NoBrowser),
            preview_ttl: Duration::from_secs(600),
        };
        let servers = Arc::new(Servers::new(
            db.clone(),
            bus.clone(),
            vgames_desktop_lib::api::http_client().unwrap(),
            VaultHandle(vault as Arc<dyn TokenVault>),
            config,
        ));
        let paths = AppPaths {
            profile: None,
            data_dir: data_dir.to_path_buf(),
            log_dir: data_dir.join("logs"),
            cache_dir: data_dir.join("cache"),
        };
        let state = AppState::new(paths, db, bus, servers).unwrap();
        Self { state }
    }

    /// Adds and activates the server (fingerprint check, trust bundle download).
    pub async fn add_server(&self, url: &str) {
        let preview = self.state.servers.preview(url, None).await.unwrap();
        self.state
            .servers
            .confirm(preview.preview_id)
            .await
            .unwrap();
    }

    pub async fn limit(&self, kib: u32) {
        self.settings(Some(kib)).await;
    }

    pub async fn unlimited(&self) {
        self.settings(None).await;
    }

    async fn settings(&self, bandwidth_limit_kib: Option<u32>) {
        self.state
            .downloads
            .set_settings(DownloadSettings {
                bandwidth_limit_kib,
                concurrent_installs: 1,
            })
            .await
            .unwrap();
    }
}
