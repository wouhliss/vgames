//! Shared application state.

use std::{ops::Deref, sync::Arc, time::Duration};

use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use tokio::sync::watch;

use crate::{config::Config, http::ratelimit::RateLimits, realtime::hub::Hub, secret::Secret};

#[derive(Clone)]
pub struct AppState(Arc<AppStateInner>);

pub struct AppStateInner {
    pub config: Config,
    pub db: PgPool,
    /// Outbound HTTP (Discord, IGDB, Steam). Never used to fetch user-supplied URLs.
    pub http: reqwest::Client,
    pub keys: DerivedKeys,
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

impl AppState {
    pub fn new(config: Config, db: PgPool) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("vgames-api/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let keys = DerivedKeys::derive(config.server_secret.expose());
        Ok(Self(Arc::new(AppStateInner {
            config,
            db,
            http,
            keys,
            limits: RateLimits::new(),
            realtime: Hub::new(crate::social::realtime_handlers()),
            realtime_ready: watch::Sender::new(false),
            shutdown: CancellationToken::new(),
            instance_id: format!("{}-{}", hostname_hint(), uuid::Uuid::now_v7()),
        })))
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
