//! The upload engine against Agent 1's actual fs object-storage wire protocol.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;
use vgames_api::config::{Config, DiscordConfig, LogFormat, StorageConfig};
use vgames_api::secret::Secret;
use vgames_api::storage::BucketKind;
use vgames_pack::scan::{FsReader, scan};
use vgames_pack::{PACK_SIZE, PackSource, Plan};
use vgames_proto::versions::{UploadMethod, UploadTarget};
use vgames_transfer::download::RemoteError;
use vgames_transfer::upload::{UploadApi, UploadControl, UploadOptions, run};

struct FsApi {
    storage: Arc<vgames_api::storage::Storage>,
}

impl UploadApi for FsApi {
    async fn pack_upload_target(
        &self,
        version_id: Uuid,
        pack: u32,
    ) -> Result<UploadTarget, RemoteError> {
        let name = format!("v1/{version_id}/packs/{pack:05}.pack");
        let signed = self
            .storage
            .sign_resumable_start(
                BucketKind::Packages,
                &name,
                Duration::from_secs(300),
                "application/octet-stream",
                1,
                PACK_SIZE,
            )
            .await
            .map_err(|_| RemoteError::fatal("cannot sign pack upload"))?;
        Ok(UploadTarget {
            url: signed.url,
            method: UploadMethod::Post,
            headers: signed.headers.into_iter().collect(),
            expires_at: signed.expires_at,
        })
    }
}

fn source(root: &Path) -> PackSource {
    let scanned = scan(root).unwrap();
    assert!(scanned.rejected.is_empty());
    let plan = Arc::new(Plan::new(scanned.files, scanned.directories).unwrap());
    let packing = Arc::new(plan.raw_packing(PACK_SIZE).unwrap());
    PackSource::new(plan, packing, Arc::new(FsReader::new(root)))
}

#[tokio::test]
async fn sends_a_real_pack_to_the_fs_backend() {
    let dir = tempfile::tempdir().unwrap();
    let source_dir = dir.path().join("source");
    let storage_dir = dir.path().join("storage");
    std::fs::create_dir(&source_dir).unwrap();
    let bytes = vec![b'v'; 1024 * 1024 + 1];
    std::fs::write(source_dir.join("game.bin"), &bytes).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let config = Config {
        bind_addr: listener.local_addr().unwrap(),
        public_url: format!("{origin}/").parse().unwrap(),
        server_name: "Upload test".into(),
        server_id: Uuid::now_v7(),
        database_url: Secret::new(String::new()),
        database_migration_url: Secret::new(String::new()),
        db_max_connections: 1,
        log_filter: "off".into(),
        log_format: LogFormat::Pretty,
        root_public_key: vgames_core::SecretKey::from_seed(&[0; 32]).public_key(),
        server_secret: Secret::new(vec![0; 32]),
        discord: DiscordConfig::Fake,
        bootstrap_owner_discord_id: None,
        storage: StorageConfig::Fs {
            root: storage_dir.clone(),
            signing_key: Secret::new(vec![0; 32]),
        },
        igdb: None,
        steam_metadata_enabled: false,
        save_quota_bytes_per_package: 0,
        signed_url_ttl: Duration::from_secs(300),
        worker_concurrency: 1,
        trust_proxy_headers: false,
        admin_dist: None,
    };
    let pool = PgPoolOptions::new()
        .connect_lazy_with(PgConnectOptions::new().host("127.0.0.1").username("test"));
    let state = vgames_api::AppState::new(config, pool).unwrap();
    let api = Arc::new(FsApi {
        storage: Arc::clone(&state.storage),
    });
    let router = vgames_api::storage::fs::routes(&state).with_state(state);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut options = UploadOptions::default();
    options.client_options.use_system_proxy = false;
    let version_id = Uuid::now_v7();
    run(
        source(&source_dir),
        version_id,
        api,
        dir.path().join("resume.json"),
        &options,
        &UploadControl::new(),
    )
    .await
    .unwrap();
    let object = storage_dir.join(format!("packages/v1/{version_id}/packs/00000.pack"));
    assert_eq!(std::fs::read(object).unwrap(), bytes);
    server.abort();
}
