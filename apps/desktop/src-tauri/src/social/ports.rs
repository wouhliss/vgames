//! What the social core needs from the rest of the launcher, as small interfaces.
//!
//! - [`SessionSlot`]: the signed-in session on the active server. Agent 2's session manager
//!   (A2-T07) calls [`SessionSlot::set`] on sign-in, token refresh, server switch and
//!   sign-out, and registers [`SessionSlot::on_unauthorized`] to refresh a token the server
//!   refused. Until then the slot stays empty and social features report `signed_out`.
//! - [`IdleSource`]: how long the user has been away from mouse and keyboard.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

/// The signed-in session on the active server.
#[derive(Clone)]
pub struct ServerSession {
    pub server_id: Uuid,
    pub user_id: Uuid,
    /// Normalized base URL (`https://host[:port]/`).
    pub base_url: Url,
    /// Bearer access token; refreshed tokens replace the whole session in the slot.
    pub access_token: Arc<Zeroizing<String>>,
}

impl ServerSession {
    /// Same server and user (a refreshed token does not count as a change).
    pub fn same_identity(&self, other: &ServerSession) -> bool {
        self.server_id == other.server_id
            && self.user_id == other.user_id
            && self.base_url == other.base_url
    }
}

impl fmt::Debug for ServerSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerSession")
            .field("server_id", &self.server_id)
            .field("user_id", &self.user_id)
            .field("base_url", &self.base_url.as_str())
            .field("access_token", &"<redacted>")
            .finish()
    }
}

type UnauthorizedHook = Box<dyn Fn(Uuid) + Send + Sync>;

/// Holds the current session and tells subscribers when it changes.
#[derive(Clone)]
pub struct SessionSlot {
    tx: Arc<watch::Sender<Option<ServerSession>>>,
    unauthorized: Arc<Mutex<Option<UnauthorizedHook>>>,
    /// How long a refused call waits for a refreshed token before giving up (zero: no
    /// retry). Set by whoever refreshes tokens (the session bridge).
    refresh_wait: Arc<Mutex<Duration>>,
}

impl Default for SessionSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionSlot {
    pub fn new() -> Self {
        Self {
            tx: Arc::new(watch::channel(None).0),
            unauthorized: Arc::new(Mutex::new(None)),
            refresh_wait: Arc::new(Mutex::new(Duration::ZERO)),
        }
    }

    /// Lets refused calls wait up to `wait` for a refreshed token and retry once.
    pub fn set_refresh_wait(&self, wait: Duration) {
        *self
            .refresh_wait
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = wait;
    }

    /// Reports `stale` as refused and waits for a new token for the same server and user.
    /// `None` when nothing refreshes it in time, or the session changed identity or ended.
    pub(crate) async fn refreshed(&self, stale: &ServerSession) -> Option<ServerSession> {
        let wait = *self
            .refresh_wait
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut rx = self.tx.subscribe();
        self.report_unauthorized(stale.server_id);
        if wait.is_zero() {
            return None;
        }
        tokio::time::timeout(wait, async {
            loop {
                {
                    let now = rx.borrow_and_update();
                    match now.as_ref() {
                        Some(s) if !s.same_identity(stale) => return None,
                        None => return None,
                        Some(s) if s.access_token != stale.access_token => return Some(s.clone()),
                        Some(_) => {}
                    }
                }
                if rx.changed().await.is_err() {
                    return None;
                }
            }
        })
        .await
        .ok()
        .flatten()
    }

    /// Replaces the session (`None` when signed out).
    pub fn set(&self, session: Option<ServerSession>) {
        self.tx.send_replace(session);
    }

    pub fn current(&self) -> Option<ServerSession> {
        self.tx.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<ServerSession>> {
        self.tx.subscribe()
    }

    /// Called with the server id whenever that server refuses the access token.
    pub fn on_unauthorized(&self, hook: impl Fn(Uuid) + Send + Sync + 'static) {
        *self
            .unauthorized
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(hook));
    }

    pub(crate) fn report_unauthorized(&self, server_id: Uuid) {
        tracing::info!(%server_id, "the server refused the access token");
        if let Some(hook) = self
            .unauthorized
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            hook(server_id);
        }
    }
}

/// A package's local state for an invite (05-social §5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameCheck {
    /// Installed and current: ready to join.
    Current,
    /// Not installed: the install dialog opens.
    Missing,
    /// Installed, but an update is needed: the update dialog opens.
    Outdated,
    /// No build this computer can run.
    NoBuildForPlatform,
}

/// Why a join launch did not start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaunchError {
    #[error("launching games is not available yet")]
    Unavailable,
    #[error("the game is not installed")]
    NotInstalled,
    #[error("launch failed: {0}")]
    Failed(String),
}

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// What the invite flow needs from the library and the launcher (Agent 2: A2-T08 install
/// state, A2-T09 launch with join arguments).
pub trait Games: Send + Sync + 'static {
    /// The package's local state on `server`.
    fn check(&self, server: Uuid, package: Uuid) -> BoxFuture<'_, GameCheck>;
    /// Launches the manifest's `multiplayer.join` target with `join_secret` substituted as a
    /// whole argument (already validated; `None` = normal launch without join arguments).
    fn launch_join(
        &self,
        server: Uuid,
        package: Uuid,
        join_secret: Option<String>,
    ) -> BoxFuture<'_, Result<(), LaunchError>>;
}

/// Install state from the local `installs` table (an installed row counts as current: update
/// detection needs A2-T08's platform choice). Cannot launch; [`LauncherGames`] can.
pub struct LocalLibrary {
    pub db: crate::db::Db,
}

impl Games for LocalLibrary {
    fn check(&self, server: Uuid, package: Uuid) -> BoxFuture<'_, GameCheck> {
        Box::pin(async move {
            let state = self
                .db
                .call(move |c| {
                    use crate::db::rusqlite::OptionalExtension as _;
                    Ok(c.query_row(
                        "SELECT state FROM installs WHERE server_id = ?1 AND package_id = ?2",
                        [server.to_string(), package.to_string()],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?)
                })
                .await;
            match state {
                Ok(Some(s)) if s == "installed" => GameCheck::Current,
                Ok(Some(s)) if s == "updating" || s == "repairing" || s == "moving" => {
                    GameCheck::Outdated
                }
                Ok(_) => GameCheck::Missing,
                Err(error) => {
                    tracing::warn!(%error, "cannot read the install state");
                    GameCheck::Missing
                }
            }
        })
    }

    fn launch_join(
        &self,
        _server: Uuid,
        _package: Uuid,
        _join_secret: Option<String>,
    ) -> BoxFuture<'_, Result<(), LaunchError>> {
        Box::pin(async { Err(LaunchError::Unavailable) })
    }
}

/// The real port: [`LocalLibrary`] install state, launches through Agent 2's launcher (rate
/// limit, pre-launch checks, `multiplayer.join` target with the secret as a whole argument).
pub struct LauncherGames<T> {
    pub library: LocalLibrary,
    pub launcher: Arc<crate::launch::orchestrate::Launcher<T>>,
}

impl<T: crate::launch::orchestrate::TrustSource> Games for LauncherGames<T> {
    fn check(&self, server: Uuid, package: Uuid) -> BoxFuture<'_, GameCheck> {
        self.library.check(server, package)
    }

    fn launch_join(
        &self,
        server: Uuid,
        package: Uuid,
        join_secret: Option<String>,
    ) -> BoxFuture<'_, Result<(), LaunchError>> {
        use crate::launch::TargetChoice;
        use crate::launch::orchestrate::LaunchError as Refused;
        Box::pin(async move {
            let package = crate::events::PackageRef {
                server_id: server,
                package_id: package,
            };
            let choice = TargetChoice::for_invite(join_secret.as_deref());
            match self.launcher.launch(package, choice).await {
                Ok(_) => Ok(()),
                Err(Refused::NotInstalled | Refused::Incomplete) => Err(LaunchError::NotInstalled),
                Err(other) => Err(LaunchError::Failed(other.to_string())),
            }
        })
    }
}

/// Time since the last keyboard or mouse input, when the OS tells us.
pub trait IdleSource: Send + Sync + 'static {
    fn idle_for(&self) -> Option<Duration>;
}

/// The operating system's input idle time.
pub struct SystemIdle;

impl IdleSource for SystemIdle {
    fn idle_for(&self) -> Option<Duration> {
        super::idle::system_idle()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn session(user: u128, token: &str) -> ServerSession {
        ServerSession {
            server_id: Uuid::from_u128(1),
            user_id: Uuid::from_u128(user),
            base_url: "https://vgames.test/".parse().unwrap(),
            access_token: Arc::new(Zeroizing::new(token.to_owned())),
        }
    }

    #[tokio::test]
    async fn refused_tokens_wait_for_a_refresh_of_the_same_session_only() {
        let slot = SessionSlot::new();
        let stale = session(2, "old");
        slot.set(Some(stale.clone()));
        // No refresher configured: report and give up at once.
        assert!(slot.refreshed(&stale).await.is_none());

        slot.set_refresh_wait(Duration::from_secs(5));
        let s2 = slot.clone();
        slot.on_unauthorized(move |_| s2.set(Some(session(2, "new"))));
        let fresh = slot.refreshed(&stale).await.unwrap();
        assert_eq!(fresh.access_token.as_str(), "new");

        // The refresher signs another user in: not a retry for this call.
        let s3 = slot.clone();
        slot.on_unauthorized(move |_| s3.set(Some(session(3, "other"))));
        assert!(slot.refreshed(&fresh).await.is_none());

        // Signed out.
        let s4 = slot.clone();
        slot.on_unauthorized(move |_| s4.set(None));
        slot.set(Some(stale.clone()));
        assert!(slot.refreshed(&stale).await.is_none());

        // Nobody refreshes: the wait ends.
        slot.set_refresh_wait(Duration::from_millis(100));
        slot.on_unauthorized(|_| {});
        slot.set(Some(stale.clone()));
        assert!(slot.refreshed(&stale).await.is_none());
    }
}
