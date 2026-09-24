//! Shared application state.

use std::{ops::Deref, sync::Arc, time::Duration};

use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use tokio::sync::watch;

use crate::{
    config::{Config, StorageConfig},
    http::ratelimit::RateLimits,
    realtime::hub::Hub,
    secret::Secret,
    storage::{Storage, StorageError, fs::FsStore},
};

#[derive(Clone)]
pub struct AppState(Arc<AppStateInner>);

pub struct AppStateInner {
    pub config: Config,
    pub db: PgPool,
    /// Outbound HTTP (Discord, IGDB, Steam). Never used to fetch user-supplied URLs.
    pub http: reqwest::Client,
    pub keys: DerivedKeys,
    /// IGDB / Steam clients and the SSRF-safe image fetcher.
    pub metadata: Arc<crate::metadata::Providers>,
    /// Object storage (GCS or fs).
    pub storage: Arc<Storage>,
    pub limits: RateLimits,
    /// Sockets connected to this instance.
    pub realtime: Hub,
    /// Becomes `true` once this instance is `LISTEN`ing for realtime events.
    pub realtime_ready: watch::Sender<bool>,
    /// Cancelled on shutdown: long-lived tasks and sockets watch it.
    pub shutdown: CancellationToken,
    /// Identifies this process among API instances (realtime fan-out, job leases).
    pub instance_id: String,
}

/// Purpose-specific keys derived from `VGAMES_SERVER_SECRET` (BLAKE3 `derive_key`).
pub struct DerivedKeys {
    pub cursor: Secret<[u8; 32]>,
}

impl DerivedKeys {
    pub fn derive(server_secret: &[u8]) -> Self {
        Self {
            cursor: Secret::new(blake3::derive_key(
                "vgames 2026-09 cursor hmac v1",
                server_secret,
            )),
        }
    }
}

/// Why the application state could not be built.
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("http client: {0}")]
    Http(#[from] reqwest::Error),
    #[error("object storage: {0}")]
    Storage(#[from] StorageError),
    #[error("the GCS backend must be created with AppState::connect")]
    NeedsAsync,
}

impl AppState {
    /// Builds the state; storage is created from the configuration (async for GCS).
    pub async fn connect(config: Config, db: PgPool) -> Result<Self, InitError> {
        let storage = Storage::from_config(&config).await?;
        Self::with_storage(config, db, storage)
    }

    /// Synchronous constructor for the `fs` storage backend (development and tests).
    pub fn new(config: Config, db: PgPool) -> Result<Self, InitError> {
        let storage = match &config.storage {
            StorageConfig::Fs { root, signing_key } => Storage::Fs(FsStore::new(
                root.clone(),
                signing_key.expose(),
                config.public_origin(),
            )?),
            StorageConfig::Gcs { .. } => return Err(InitError::NeedsAsync),
        };
        Self::with_storage(config, db, storage)
    }

    pub fn with_storage(config: Config, db: PgPool, storage: Storage) -> Result<Self, InitError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("vgames-api/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let keys = DerivedKeys::derive(config.server_secret.expose());
        let metadata = Arc::new(crate::metadata::Providers::from_config(&config)?);
        Ok(Self(Arc::new(AppStateInner {
            config,
            db,
            http,
            keys,
            metadata,
            storage: Arc::new(storage),
            limits: RateLimits::new(),
            realtime: Hub::new(crate::social::realtime_handlers()),
            realtime_ready: watch::Sender::new(false),
            shutdown: CancellationToken::new(),
            instance_id: format!("{}-{}", hostname_hint(), uuid::Uuid::now_v7()),
        })))
    }
}

impl AppState {
    /// Replaces the metadata providers (tests point them at mock servers). Only possible
    /// before the state is shared; otherwise the state is returned unchanged as `Err`.
    pub fn with_metadata(self, providers: crate::metadata::Providers) -> Result<Self, Self> {
        match Arc::try_unwrap(self.0) {
            Ok(mut inner) => {
                inner.metadata = Arc::new(providers);
                Ok(Self(Arc::new(inner)))
            }
            Err(shared) => Err(Self(shared)),
        }
    }
}

impl Deref for AppState {
    type Target = AppStateInner;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

fn hostname_hint() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty() && h.len() <= 64)
        .unwrap_or_else(|| "api".to_string())
}
