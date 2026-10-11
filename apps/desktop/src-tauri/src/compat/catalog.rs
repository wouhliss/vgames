//! The signed runtime catalog (09-compatibility §5, GAME-03).
//!
//! `runtimes.json` and its `.minisig` come from the project's `runtimes` GitHub
//! release (INT-08's `release-runtimes.yml`). Rules:
//!
//! - Verified with `vgames_core::runtimes::verify_catalog` under the key compiled
//!   in from `runtimes/runtime-catalog.pub` (`build.rs`). Without that file the
//!   manager is off ([`CatalogStatus::Off`]). Release builds trust only the
//!   compiled-in key and URL; debug builds accept [`CATALOG_URL_ENV`] and
//!   [`CATALOG_PUBKEY_ENV`] for tests, like `VGAMES_PROFILE`.
//! - The highest accepted version is kept **per key id** (F2), with the verified
//!   bytes and signature, which are verified again when read back.
//! - Refreshed on startup, on a server switch and before a compat launch, at most
//!   every [`REFRESH_EVERY`] (a stored timestamp, never a timer).
//! - Both bodies are capped (`MAX_CATALOG_BYTES`, `MAX_SIGNATURE_BYTES`) and every
//!   request has a timeout. Redirects (GitHub serves assets from another host) are
//!   followed only to `https` in release builds, at most [`MAX_REDIRECTS`] times.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use specta::Type;
use url::Url;
use vgames_core::runtimes::{
    Catalog, MAX_CATALOG_BYTES, MAX_SIGNATURE_BYTES, RuntimesError, key_id, verify_catalog,
};

use crate::db::{Db, DbError, now_unix};

/// Where release builds fetch the catalog; the signature is `<url>.minisig`.
pub const DEFAULT_CATALOG_URL: &str =
    "https://github.com/wouhliss/vgames/releases/download/runtimes/runtimes.json";
/// Debug builds only: another catalog URL (tests, local catalogs).
pub const CATALOG_URL_ENV: &str = "VGAMES_RUNTIME_CATALOG_URL";
/// Debug builds only: another public key (the `.pub` file's contents or its base64 line).
pub const CATALOG_PUBKEY_ENV: &str = "VGAMES_RUNTIME_CATALOG_TEST_PUBKEY";
/// A catalog is fetched again at most this often.
pub const REFRESH_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_REDIRECTS: usize = 5;

/// `runtimes/runtime-catalog.pub` as `build.rs` found it (empty without the file).
const COMPILED_KEY: &str = include_str!(concat!(env!("OUT_DIR"), "/runtime-catalog.pub"));

/// Where the catalog comes from and the key that must have signed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogSource {
    pub url: Url,
    pub signature_url: Url,
    pub public_key: String,
    pub key_id: String,
}

impl CatalogSource {
    /// The compiled-in key and URL, with the debug-only overrides from the environment.
    /// `None`: no key (the manager is off).
    pub fn from_build() -> Option<Self> {
        Self::resolve(COMPILED_KEY, |name| std::env::var(name).ok())
    }

    /// [`Self::from_build`] with the compiled key and the environment passed in.
    pub fn resolve(compiled_key: &str, env: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let debug = cfg!(debug_assertions);
        let override_of = |name: &str| {
            if debug {
                env(name).filter(|v| !v.trim().is_empty())
            } else {
                None
            }
        };
        let public_key = override_of(CATALOG_PUBKEY_ENV).unwrap_or_else(|| compiled_key.to_owned());
        if public_key.trim().is_empty() {
            return None;
        }
        let key_id = match key_id(&public_key) {
            Ok(id) => id,
            Err(error) => {
                tracing::error!(%error, "the runtime-catalog key is unusable; compatibility runtimes are off");
                return None;
            }
        };
        let url_text =
            override_of(CATALOG_URL_ENV).unwrap_or_else(|| DEFAULT_CATALOG_URL.to_owned());
        let url = Url::parse(&url_text)
            .ok()
            .filter(|u| u.scheme() == "https" || (debug && u.scheme() == "http"))?;
        let signature_url = Url::parse(&format!("{url}.minisig")).ok()?;
        Some(Self {
            url,
            signature_url,
            public_key,
            key_id,
        })
    }
}

/// What Settings → Compatibility shows about the catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CatalogStatus {
    /// This build has no runtime-catalog key.
    Off,
    /// Never fetched (no catalog stored for this key yet).
    Missing,
    /// A verified catalog is in use.
    Ready {
        version: u64,
        /// Unix seconds of the last successful check.
        checked_at: i64,
    },
    /// The last refresh failed; a stored catalog, if any, stays in use.
    Failed {
        reason: String,
        stored_version: Option<u64>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("this build has no runtime-catalog key")]
    Off,
    #[error("the runtime catalog could not be downloaded: {0}")]
    Network(String),
    #[error("the runtime catalog is larger than allowed")]
    TooLarge,
    #[error(transparent)]
    Invalid(#[from] RuntimesError),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// A catalog this launcher accepted, with where it stands.
#[derive(Clone, Debug)]
pub struct Accepted {
    pub catalog: Arc<Catalog>,
    pub key_id: String,
}

/// The runtime catalog: fetch, verify, persist per key, serve.
pub struct RuntimeCatalog {
    source: Option<CatalogSource>,
    db: Db,
    http: reqwest::Client,
    current: Mutex<Option<Accepted>>,
    last_error: Mutex<Option<String>>,
    /// One refresh at a time.
    refreshing: tokio::sync::Mutex<()>,
}

/// The HTTP client for catalog files: timeouts, bounded `https`-only redirects.
pub fn http_client() -> reqwest::Result<reqwest::Client> {
    let allow_http = cfg!(debug_assertions);
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if attempt.url().scheme() == "https"
                || (allow_http && attempt.url().scheme() == "http")
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .user_agent(concat!("vgames-launcher/", env!("CARGO_PKG_VERSION")))
        .build()
}

impl RuntimeCatalog {
    pub fn new(source: Option<CatalogSource>, db: Db, http: reqwest::Client) -> Self {
        Self {
            source,
            db,
            http,
            current: Mutex::new(None),
            last_error: Mutex::new(None),
            refreshing: tokio::sync::Mutex::new(()),
        }
    }

    pub fn source(&self) -> Option<&CatalogSource> {
        self.source.as_ref()
    }

    /// The catalog in use, loading (and verifying again) the stored one on first use.
    pub async fn current(&self) -> Result<Option<Accepted>, CatalogError> {
        if let Some(c) = lock(&self.current).clone() {
            return Ok(Some(c));
        }
        let Some(source) = &self.source else {
            return Err(CatalogError::Off);
        };
        let Some(row) = stored(&self.db, &source.key_id).await? else {
            return Ok(None);
        };
        // The database is not trusted more than the network: verify again.
        match verify_catalog(&row.catalog, &row.signature, &source.public_key, None) {
            Ok(verified) => {
                let accepted = Accepted {
                    catalog: Arc::new(verified.catalog),
                    key_id: source.key_id.clone(),
                };
                *lock(&self.current) = Some(accepted.clone());
                Ok(Some(accepted))
            }
            Err(error) => {
                tracing::warn!(%error, "ignoring a stored runtime catalog that no longer verifies");
                Ok(None)
            }
        }
    }

    /// Fetches the catalog unless the last successful check is younger than
    /// [`REFRESH_EVERY`]. Returns the catalog in use afterwards.
    pub async fn refresh_if_due(&self) -> Result<Option<Accepted>, CatalogError> {
        let source = self.source.as_ref().ok_or(CatalogError::Off)?;
        let checked = stored_checked_at(&self.db, &source.key_id).await?;
        let due = checked.is_none_or(|at| {
            let age = now_unix().saturating_sub(at);
            age < 0 || u64::try_from(age).unwrap_or(0) >= REFRESH_EVERY.as_secs()
        });
        if due {
            self.refresh().await
        } else {
            self.current().await
        }
    }

    /// Fetches, verifies and stores the catalog now.
    pub async fn refresh(&self) -> Result<Option<Accepted>, CatalogError> {
        let source = self.source.clone().ok_or(CatalogError::Off)?;
        let _one = self.refreshing.lock().await;
        let result = self.fetch_and_accept(&source).await;
        *lock(&self.last_error) = result.as_ref().err().map(ToString::to_string);
        if let Err(error) = &result {
            tracing::warn!(%error, "runtime catalog refresh failed");
        }
        result?;
        self.current().await
    }

    async fn fetch_and_accept(&self, source: &CatalogSource) -> Result<(), CatalogError> {
        let bytes = fetch_capped(&self.http, &source.url, MAX_CATALOG_BYTES).await?;
        let signature =
            fetch_capped(&self.http, &source.signature_url, MAX_SIGNATURE_BYTES).await?;
        let signature = String::from_utf8(signature)
            .map_err(|_| RuntimesError::SignatureMalformed("not UTF-8".into()))?;
        let seen = stored_version(&self.db, &source.key_id).await?;
        let verified = verify_catalog(&bytes, &signature, &source.public_key, seen)?;
        let key = source.key_id.clone();
        let version = verified.catalog.version;
        let now = now_unix();
        let version_sql = i64::try_from(version).map_err(|_| RuntimesError::VersionZero)?;
        let replaced = self
            .db
            .call(move |conn| {
                // Guarded: a concurrent writer with a higher version wins.
                Ok(conn.execute(
                    "INSERT INTO compat_catalogs (key_id, version, catalog, signature, accepted_at, checked_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                     ON CONFLICT (key_id) DO UPDATE SET
                         version = excluded.version, catalog = excluded.catalog,
                         signature = excluded.signature,
                         accepted_at = CASE WHEN excluded.version > compat_catalogs.version
                             THEN excluded.accepted_at ELSE compat_catalogs.accepted_at END,
                         checked_at = excluded.checked_at
                     WHERE excluded.version >= compat_catalogs.version",
                    params![key, version_sql, bytes, signature, now],
                )?)
            })
            .await?;
        if replaced == 0 {
            // Someone stored a higher version meanwhile; that one stays.
            let seen = stored_version(&self.db, &source.key_id)
                .await?
                .unwrap_or(version);
            return Err(RuntimesError::Rollback { got: version, seen }.into());
        }
        *lock(&self.current) = Some(Accepted {
            catalog: Arc::new(verified.catalog),
            key_id: source.key_id.clone(),
        });
        tracing::info!(version, key_id = %source.key_id, newer = verified.newer, "runtime catalog verified");
        Ok(())
    }

    /// What the UI shows.
    pub async fn status(&self) -> CatalogStatus {
        let Some(source) = &self.source else {
            return CatalogStatus::Off;
        };
        let stored_version = stored_version(&self.db, &source.key_id)
            .await
            .ok()
            .flatten();
        if let Some(reason) = lock(&self.last_error).clone() {
            return CatalogStatus::Failed {
                reason,
                stored_version,
            };
        }
        match (
            stored_version,
            stored_checked_at(&self.db, &source.key_id)
                .await
                .ok()
                .flatten(),
        ) {
            (Some(version), Some(checked_at)) => CatalogStatus::Ready {
                version,
                checked_at,
            },
            _ => CatalogStatus::Missing,
        }
    }
}

impl RuntimeCatalog {
    /// Refreshes on startup and on every server switch (when due), until `cancel`.
    pub fn spawn_refresher(
        self: &Arc<Self>,
        bus: &crate::events::EventBus,
        cancel: tokio_util::sync::CancellationToken,
    ) {
        if self.source.is_none() {
            tracing::info!("no runtime-catalog key in this build; compatibility runtimes are off");
            return;
        }
        let this = Arc::clone(self);
        let mut events = bus.subscribe();
        tauri::async_runtime::spawn(async move {
            let _ = this.refresh_if_due().await;
            loop {
                tokio::select! {
                    () = cancel.cancelled() => return,
                    event = events.recv() => match event {
                        Ok(crate::events::AppEvent::ServerSwitched(_)) => {
                            let _ = this.refresh_if_due().await;
                        }
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    },
                }
            }
        });
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// GET with a body cap enforced while reading.
async fn fetch_capped(
    http: &reqwest::Client,
    url: &Url,
    cap: usize,
) -> Result<Vec<u8>, CatalogError> {
    let net = |e: reqwest::Error| CatalogError::Network(e.without_url().to_string());
    let mut response = http.get(url.clone()).send().await.map_err(net)?;
    let status = response.status();
    if !status.is_success() {
        return Err(CatalogError::Network(format!("HTTP {}", status.as_u16())));
    }
    if response
        .content_length()
        .is_some_and(|len| len > cap as u64)
    {
        return Err(CatalogError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(net)? {
        if body.len() + chunk.len() > cap {
            return Err(CatalogError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

struct StoredCatalog {
    catalog: Vec<u8>,
    signature: String,
}

async fn stored(db: &Db, key: &str) -> Result<Option<StoredCatalog>, DbError> {
    let key = key.to_owned();
    db.call(move |conn| {
        Ok(conn
            .query_row(
                "SELECT catalog, signature FROM compat_catalogs WHERE key_id = ?1",
                [key],
                |r| {
                    Ok(StoredCatalog {
                        catalog: r.get(0)?,
                        signature: r.get(1)?,
                    })
                },
            )
            .optional()?)
    })
    .await
}

async fn stored_version(db: &Db, key: &str) -> Result<Option<u64>, DbError> {
    let key = key.to_owned();
    db.call(move |conn| {
        let v: Option<i64> = conn
            .query_row(
                "SELECT version FROM compat_catalogs WHERE key_id = ?1",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.and_then(|v| u64::try_from(v).ok()))
    })
    .await
}

async fn stored_checked_at(db: &Db, key: &str) -> Result<Option<i64>, DbError> {
    let key = key.to_owned();
    db.call(move |conn| {
        Ok(conn
            .query_row(
                "SELECT checked_at FROM compat_catalogs WHERE key_id = ?1",
                [key],
                |r| r.get(0),
            )
            .optional()?)
    })
    .await
}

#[cfg(test)]
mod tests;
