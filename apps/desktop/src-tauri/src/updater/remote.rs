//! Network side of the updater: find, download and verify an update, and fetch
//! the player changelog. Independent of the Tauri runtime type, so the tests run
//! it against a local update server with throwaway keys (`tests.rs`).
//!
//! Verification is layered:
//! - every URL (endpoint, redirect target, download) is HTTPS; loopback HTTP is
//!   accepted only when [`Transport::allow_loopback_http`] is set (debug builds);
//! - only a strictly greater version is offered ([`policy::is_update`]);
//! - the plugin checks the minisign signature of the artifact against the public
//!   key compiled into the launcher, and (`requireSignedVersion`) that the signed
//!   trusted comment names the announced version, so an old signed build cannot be
//!   replayed under a new version number;
//! - downloads larger than [`MAX_UPDATE_BYTES`] are abandoned.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tauri_plugin_updater::{Update, UpdaterBuilder};
use tokio::sync::Notify;
use url::Url;

use super::policy::{self, PolicyError, ReleaseNotes};

/// Where `latest.json` is published (08-release §2). Must match
/// `plugins.updater.endpoints` in `tauri.conf.json` (a test checks it).
pub const LATEST_URL: &str =
    "https://github.com/wouhliss/vgames/releases/latest/download/latest.json";
/// Cumulative player changelog published with every release (08-release §3.3).
pub const CHANGELOG_URL: &str =
    "https://github.com/wouhliss/vgames/releases/latest/download/changelog-user.json";
/// Every request of the updater.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout of the artifact download (installers are tens of MiB).
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Largest update artifact accepted.
pub const MAX_UPDATE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

/// Which URLs may be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transport {
    /// `http://localhost` and `http://127.0.0.1` too (local tests only).
    pub allow_loopback_http: bool,
}

impl Transport {
    /// HTTPS only in release builds; debug builds also accept loopback HTTP.
    pub const fn for_build() -> Self {
        Self {
            allow_loopback_http: cfg!(debug_assertions),
        }
    }

    fn check(self, url: &Url) -> Result<(), PolicyError> {
        policy::require_https(url, self.allow_loopback_http)
    }

    /// Redirects are followed only to URLs this transport accepts, at most 5.
    fn redirect_policy(self) -> reqwest::redirect::Policy {
        reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if self.check(attempt.url()).is_err() {
                attempt.error("redirect to a URL that is not HTTPS")
            } else {
                attempt.follow()
            }
        })
    }

    fn configure(self, client: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        client
            .https_only(!self.allow_loopback_http)
            .redirect(self.redirect_policy())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// Network, parsing, or verification failure reported by the updater plugin.
    /// A bad signature or a signed-version mismatch lands here.
    #[error(transparent)]
    Updater(#[from] tauri_plugin_updater::Error),
    #[error("the update is larger than {MAX_UPDATE_BYTES} bytes")]
    TooLarge,
    #[error("changelog request failed: {0}")]
    Http(#[from] reqwest::Error),
}

impl UpdateError {
    /// Whether the failure means the update itself is untrustworthy (never retried
    /// silently) rather than a network problem.
    pub fn is_integrity(&self) -> bool {
        use tauri_plugin_updater::Error as E;
        match self {
            Self::Policy(_) | Self::TooLarge => true,
            Self::Updater(e) => matches!(
                e,
                E::Minisign(_)
                    | E::Base64(_)
                    | E::SignatureUtf8(_)
                    | E::MissingSignedVersion
                    | E::SignedVersionMismatch { .. }
                    | E::InsecureTransportProtocol
            ),
            Self::Http(_) => false,
        }
    }
}

/// Asks `endpoint` for a newer release. `builder` comes from
/// `app.updater_builder()` (plugin config: public key, `requireSignedVersion`).
pub async fn find_update(
    builder: UpdaterBuilder,
    endpoint: &Url,
    transport: Transport,
) -> Result<Option<Update>, UpdateError> {
    transport.check(endpoint)?;
    let updater = builder
        .endpoints(vec![endpoint.clone()])?
        // Strictly greater only: never a downgrade, never a re-install.
        .version_comparator(|current, remote| policy::is_update(&current, &remote.version))
        .timeout(HTTP_TIMEOUT)
        .configure_client(move |client| transport.configure(client))
        .build()?;
    let Some(mut update) = updater.check().await? else {
        return Ok(None);
    };
    transport.check(&update.download_url)?;
    update.timeout = Some(DOWNLOAD_TIMEOUT);
    Ok(Some(update))
}

/// Downloads the artifact and verifies its minisign signature (and signed
/// version). `progress(downloaded, total)` is called for every chunk. Nothing is
/// returned unless verification passed.
pub async fn download_verified(
    update: &Update,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<Vec<u8>, UpdateError> {
    let too_large = Arc::new(Notify::new());
    let exceeded = Arc::new(AtomicBool::new(false));
    let downloaded = Arc::new(AtomicU64::new(0));
    let (notify, flag, count) = (too_large.clone(), exceeded.clone(), downloaded.clone());
    let download = update.download(
        move |chunk, total| {
            let so_far = count
                .fetch_add(chunk as u64, Ordering::Relaxed)
                .saturating_add(chunk as u64);
            if so_far > MAX_UPDATE_BYTES || total.is_some_and(|t| t > MAX_UPDATE_BYTES) {
                flag.store(true, Ordering::Relaxed);
                // Stores a permit: the select below stops the download.
                notify.notify_one();
            }
            progress(so_far, total);
        },
        || {},
    );
    let result = tokio::select! {
        biased;
        () = too_large.notified() => return Err(UpdateError::TooLarge),
        result = download => result,
    };
    // The download may end in the same poll that crossed the cap.
    if exceeded.load(Ordering::Relaxed) {
        return Err(UpdateError::TooLarge);
    }
    Ok(result?)
}

/// Fetches `changelog-user.json`: at most 1 MiB, strictly validated.
pub async fn fetch_changelog(
    url: &Url,
    transport: Transport,
) -> Result<Vec<ReleaseNotes>, UpdateError> {
    transport.check(url)?;
    let client = transport
        .configure(reqwest::Client::builder())
        .timeout(HTTP_TIMEOUT)
        .build()?;
    let mut response = client.get(url.clone()).send().await?.error_for_status()?;
    let cap = policy::MAX_CHANGELOG_BYTES;
    if response.content_length().is_some_and(|n| n > cap as u64) {
        return Err(PolicyError::ChangelogTooLarge.into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > cap {
            return Err(PolicyError::ChangelogTooLarge.into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(policy::parse_changelog(&body)?)
}
