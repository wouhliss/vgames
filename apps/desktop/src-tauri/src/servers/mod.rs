//! Servers, root pinning and trust bundles in the launcher (A2-T07,
//! 01-security §3, 00-overview "Add server").
//!
//! - Adding a server is two steps: [`Servers::preview`] fetches the discovery
//!   document and shows the root fingerprint; [`Servers::confirm`] pins that
//!   root (TOFU, or checked against the `fp` of a `vgames://server/add` link).
//! - Every later connection ([`Servers::connect`]) compares the presented root
//!   key with the pin. A mismatch blocks the server (persisted, every request
//!   refused, `TrustProblem` event). There is no bypass: the block lifts only
//!   when the server presents the pinned key again.
//! - Trust bundles are verified with `vgames_core::trust::verify_bundle` and the
//!   highest version is stored; an older one is refused (rollback). A bundle
//!   signed by the announced next root completes a rotation and moves the pin.
//! - One server is active at a time; switching publishes `ServerSwitched`.

pub mod auth;
pub mod discovery;
pub mod store;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::Method;
use serde::{Deserialize, Serialize};
use specta::Type;
use url::Url;
use uuid::Uuid;
use vgames_core::sign::{PublicKey, Signature};
use vgames_core::trust::{
    RootPin, SignedBundle, TrustError, TrustState, VerifiedBundle, verify_bundle,
};

use crate::api::{ApiClient, ApiError, Session};
use crate::db::{Db, DbError};
use crate::error::AppError;
use crate::events::{
    AppEvent, ConnectivityChanged, EventBus, ServerSwitched, ServersChanged, TrustProblem,
    TrustProblemKind,
};
use crate::secrets::VaultHandle;
use discovery::{Discovered, discover, normalize_url};
use store::ServerRow;

/// How long a preview stays confirmable.
pub const PREVIEW_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_PREVIEWS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Admin,
    Owner,
}

/// The account signed in on a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct Account {
    pub user_id: Uuid,
    pub username: String,
    pub display_name: Option<String>,
    pub role: Role,
}

impl From<&vgames_proto::auth::User> for Account {
    fn from(user: &vgames_proto::auth::User) -> Self {
        use vgames_proto::auth::Role as P;
        Self {
            user_id: user.id,
            username: user.username.clone(),
            display_name: user.display_name.clone(),
            role: match user.role {
                P::User => Role::User,
                P::Admin => Role::Admin,
                P::Owner => Role::Owner,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationMode {
    Open,
    Allowlist,
    Closed,
}

/// A stored server as the UI sees it.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ServerProfile {
    pub id: Uuid,
    pub url: String,
    pub name: String,
    /// `VG1-XXXX-…` root key fingerprint, pinned when the server was added.
    pub fingerprint: String,
    pub active: bool,
    pub account: Option<Account>,
    /// RFC 3339.
    pub last_connected_at: Option<String>,
    /// Set while the server is blocked: the fingerprint it presented instead of the pinned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[specta(optional)]
    pub blocked_fingerprint: Option<String>,
}

impl From<ServerRow> for ServerProfile {
    fn from(row: ServerRow) -> Self {
        Self {
            id: row.id,
            url: row.url,
            name: row.name,
            fingerprint: row.root_fingerprint,
            active: row.active,
            account: row.account,
            last_connected_at: row.last_connected_at.and_then(rfc3339_from_unix),
            blocked_fingerprint: row.blocked_fingerprint,
        }
    }
}

fn rfc3339_from_unix(seconds: i64) -> Option<String> {
    time::OffsetDateTime::from_unix_timestamp(seconds)
        .ok()?
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

/// What `server_preview` shows before the user confirms.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ServerPreview {
    /// Opaque handle for `server_confirm`; the preview itself stays in Rust.
    pub preview_id: Uuid,
    pub url: String,
    pub server_id: Uuid,
    pub name: String,
    pub motd: Option<String>,
    pub fingerprint: String,
    pub registration_mode: Option<RegistrationMode>,
    /// Set when the preview came from a `vgames://server/add` link (already checked equal).
    pub expected_fingerprint: Option<String>,
}

/// Why a server cannot be previewed or added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerError {
    #[error("this is not a valid server address")]
    InvalidUrl,
    #[error("the server address must use https")]
    InsecureScheme,
    #[error("the server could not be reached: {detail}")]
    Unreachable { detail: String },
    #[error("the server did not answer in time")]
    Timeout,
    #[error("secure connection failed: {detail}")]
    Tls { detail: String },
    #[error("this address is not a vgames server")]
    NotVgames,
    #[error("this server needs launcher {min_version} or newer (this is {current_version})")]
    LauncherTooOld {
        min_version: String,
        current_version: String,
    },
    #[error("the server's key fingerprint {actual} does not match {expected}")]
    FingerprintMismatch { expected: String, actual: String },
    #[error("this server is already added")]
    AlreadyAdded { server_id: Uuid },
    #[error("the preview expired; look the server up again")]
    PreviewExpired,
}

impl ServerError {
    pub(crate) fn from_api(error: ApiError) -> Self {
        match error {
            ApiError::Timeout => Self::Timeout,
            ApiError::Tls(detail) => Self::Tls { detail },
            ApiError::Network(detail) => Self::Unreachable { detail },
            other => Self::Unreachable {
                detail: other.to_string(),
            },
        }
    }

    fn internal(error: &DbError) -> Self {
        tracing::error!(error = %crate::error::DisplayChain(error), "server store failed");
        // The UI contract has no local-failure kind yet (requested from Agent 3).
        Self::Unreachable {
            detail: "The local database failed. See the log for details.".into(),
        }
    }
}

/// Why [`Servers::connect`] did not complete.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("server not found")]
    NotFound,
    #[error("the server is offline: {0}")]
    Offline(ServerError),
    #[error("the server presented a different root key ({presented}); blocked")]
    Blocked { presented: String },
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Why the trust bundle could not be refreshed.
#[derive(Debug, thiserror::Error)]
pub enum TrustRefreshError {
    #[error("server not found")]
    NotFound,
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error("the trust bundle was refused: {0}")]
    Refused(#[from] TrustError),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Opens URLs in the system browser (a trait so tests can record calls).
pub trait BrowserOpener: Send + Sync {
    /// Returns false when no browser could be started.
    fn open(&self, url: &Url) -> bool;
}

/// The real system browser.
pub struct SystemBrowser;

impl BrowserOpener for SystemBrowser {
    fn open(&self, url: &Url) -> bool {
        match open::that_detached(url.as_str()) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "cannot open the system browser");
                false
            }
        }
    }
}

pub struct ServersConfig {
    /// `http://localhost` servers (debug builds only).
    pub allow_loopback_http: bool,
    pub launcher_version: semver::Version,
    /// Shown in the user's session list (1–64 characters).
    pub device_name: String,
    pub browser: Arc<dyn BrowserOpener>,
    pub preview_ttl: Duration,
}

impl ServersConfig {
    pub fn for_this_build(device_name: String) -> Self {
        Self {
            allow_loopback_http: cfg!(debug_assertions),
            launcher_version: semver::Version::parse(env!("CARGO_PKG_VERSION"))
                .unwrap_or_else(|_| semver::Version::new(0, 0, 0)),
            device_name,
            browser: Arc::new(SystemBrowser),
            preview_ttl: PREVIEW_TTL,
        }
    }
}

struct Preview {
    url: Url,
    discovered: Discovered,
    expected: Option<String>,
    created: Instant,
}

/// Owner of every server connection, session and trust state.
pub struct Servers {
    db: Db,
    bus: EventBus,
    http: reqwest::Client,
    vault: VaultHandle,
    config: ServersConfig,
    previews: Mutex<HashMap<Uuid, Preview>>,
    sessions: Mutex<HashMap<Uuid, Arc<Session>>>,
    trust: Mutex<HashMap<Uuid, Arc<TrustState>>>,
    flows: Mutex<HashMap<Uuid, auth::PendingFlow>>,
}

impl Servers {
    pub fn new(
        db: Db,
        bus: EventBus,
        http: reqwest::Client,
        vault: VaultHandle,
        config: ServersConfig,
    ) -> Self {
        Self {
            db,
            bus,
            http,
            vault,
            config,
            previews: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            trust: Mutex::new(HashMap::new()),
            flows: Mutex::new(HashMap::new()),
        }
    }

    /// Whether tokens fall back to a private file (no OS keychain).
    pub fn token_storage_is_fallback(&self) -> bool {
        self.vault.is_fallback()
    }

    pub async fn list(&self) -> Result<Vec<ServerProfile>, DbError> {
        let rows = self.db.call(|conn| store::list(conn)).await?;
        Ok(rows.into_iter().map(ServerProfile::from).collect())
    }

    pub async fn profile(&self, id: Uuid) -> Result<Option<ServerProfile>, DbError> {
        let row = self.db.call(move |conn| store::get(conn, id)).await?;
        Ok(row.map(ServerProfile::from))
    }

    pub async fn active_id(&self) -> Result<Option<Uuid>, DbError> {
        let row = self.db.call(|conn| store::active(conn)).await?;
        Ok(row.map(|r| r.id))
    }

    async fn row(&self, id: Uuid) -> Result<Option<ServerRow>, DbError> {
        self.db.call(move |conn| store::get(conn, id)).await
    }

    /// Step 1 of adding a server: fetch and check its discovery document.
    pub async fn preview(
        &self,
        input_url: &str,
        expected_fingerprint: Option<&str>,
    ) -> Result<ServerPreview, ServerError> {
        let url = normalize_url(input_url, self.config.allow_loopback_http)?;
        let discovered = discover(&self.http, &url, &self.config.launcher_version).await?;
        let actual = discovered.fingerprint.to_string();
        let expected = expected_fingerprint
            .map(|fp| fp.trim().to_ascii_uppercase())
            .filter(|fp| !fp.is_empty());
        if let Some(expected) = &expected
            && *expected != actual
        {
            return Err(ServerError::FingerprintMismatch {
                expected: expected.chars().take(64).collect(),
                actual,
            });
        }
        self.check_not_added(&discovered, &url).await?;

        let preview_id = Uuid::now_v7();
        let preview = ServerPreview {
            preview_id,
            url: url.to_string(),
            server_id: discovered.server_id,
            name: discovered.name.clone(),
            motd: discovered.motd.clone(),
            fingerprint: actual,
            registration_mode: discovered.registration_mode,
            expected_fingerprint: expected.clone(),
        };
        let mut previews = self.previews.lock().map_err(|_| poisoned())?;
        let ttl = self.config.preview_ttl;
        previews.retain(|_, p| p.created.elapsed() < ttl);
        while previews.len() >= MAX_PREVIEWS {
            let oldest = previews
                .iter()
                .min_by_key(|(_, p)| p.created)
                .map(|(id, _)| *id);
            match oldest {
                Some(id) => previews.remove(&id),
                None => break,
            };
        }
        previews.insert(
            preview_id,
            Preview {
                url,
                discovered,
                expected,
                created: Instant::now(),
            },
        );
        Ok(preview)
    }

    /// A server with the same id or URL is already stored: same key → already
    /// added; another key → mismatch (a new pin is never silently taken).
    async fn check_not_added(&self, discovered: &Discovered, url: &Url) -> Result<(), ServerError> {
        let id = discovered.server_id;
        let url_text = url.to_string();
        let existing = self
            .db
            .call(move |conn| store::find(conn, id, &url_text))
            .await
            .map_err(|e| ServerError::internal(&e))?;
        match existing {
            None => Ok(()),
            Some(row) if row.root_public_key == *discovered.root.as_bytes() => {
                Err(ServerError::AlreadyAdded { server_id: row.id })
            }
            Some(row) => Err(ServerError::FingerprintMismatch {
                expected: row.root_fingerprint,
                actual: discovered.fingerprint.to_string(),
            }),
        }
    }

    /// Step 2: pins the previewed root and makes the server active.
    pub async fn confirm(&self, preview_id: Uuid) -> Result<ServerProfile, ServerError> {
        let preview = self
            .previews
            .lock()
            .map_err(|_| poisoned())?
            .remove(&preview_id)
            .filter(|p| p.created.elapsed() < self.config.preview_ttl)
            .ok_or(ServerError::PreviewExpired)?;
        self.check_not_added(&preview.discovered, &preview.url)
            .await?;
        let d = &preview.discovered;
        let id = d.server_id;
        let url = preview.url.to_string();
        let name = d.name.clone();
        let key = *d.root.as_bytes();
        let fingerprint = d.fingerprint.to_string();
        self.db
            .call(move |conn| {
                store::insert_active(
                    conn,
                    &store::NewServer {
                        id,
                        url: &url,
                        name: &name,
                        root_public_key: &key,
                        root_fingerprint: &fingerprint,
                    },
                )
            })
            .await
            .map_err(|e| ServerError::internal(&e))?;
        tracing::info!(
            server_id = %id,
            fingerprint = %d.fingerprint,
            from_link = preview.expected.is_some(),
            "server added and root pinned"
        );
        self.publish_switched(Some(id));
        if let Err(error) = self.refresh_trust(id).await {
            tracing::warn!(server_id = %id, %error, "no trust bundle yet for the new server");
        }
        self.profile(id)
            .await
            .map_err(|e| ServerError::internal(&e))?
            .ok_or(ServerError::PreviewExpired)
    }

    /// Makes `id` the active server. The caller then runs [`Self::connect`].
    pub async fn switch(&self, id: Uuid) -> Result<ServerProfile, AppError> {
        let found = self
            .db
            .call(move |conn| store::set_active(conn, id))
            .await
            .map_err(|e| AppError::internal("switch servers", &e))?;
        if !found {
            return Err(AppError::NotFound);
        }
        self.publish_switched(Some(id));
        self.profile(id)
            .await
            .map_err(|e| AppError::internal("switch servers", &e))?
            .ok_or(AppError::NotFound)
    }

    /// Signs out (best effort) and forgets the server. Refused while games
    /// from it are installed.
    pub async fn remove(&self, id: Uuid) -> Result<(), AppError> {
        if self
            .row(id)
            .await
            .map_err(|e| AppError::internal("remove the server", &e))?
            .is_none()
        {
            return Err(AppError::NotFound);
        }
        self.sign_out(id).await?;
        let outcome = self
            .db
            .call(move |conn| store::remove(conn, id))
            .await
            .map_err(|e| AppError::internal("remove the server", &e))?;
        match outcome {
            store::RemoveOutcome::NotFound => Err(AppError::NotFound),
            store::RemoveOutcome::HasInstalls => Err(AppError::InvalidInput {
                field: "server_id".into(),
                message: "Uninstall this server's games before removing it.".into(),
            }),
            store::RemoveOutcome::Removed { was_active } => {
                self.forget(id);
                if was_active {
                    self.publish_switched(None);
                } else {
                    self.bus
                        .publish(AppEvent::ServersChanged(ServersChanged {}));
                }
                Ok(())
            }
        }
    }

    fn forget(&self, id: Uuid) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(&id);
        }
        if let Ok(mut trust) = self.trust.lock() {
            trust.remove(&id);
        }
        if let Ok(mut flows) = self.flows.lock() {
            flows.retain(|_, f| f.server_id != id);
        }
    }

    fn publish_switched(&self, id: Option<Uuid>) {
        self.bus
            .publish(AppEvent::ServerSwitched(ServerSwitched { server_id: id }));
        self.bus
            .publish(AppEvent::ServersChanged(ServersChanged {}));
    }

    fn session_for(&self, row: &ServerRow) -> Result<Arc<Session>, DbError> {
        let mut sessions = self.sessions.lock().map_err(|_| DbError::Closed)?;
        let session = match sessions.get(&row.id) {
            Some(session) => Arc::clone(session),
            None => {
                let base = Url::parse(&row.url).map_err(|_| DbError::Closed)?;
                let db = self.db.clone();
                let bus = self.bus.clone();
                let hook: crate::api::session::SignedOutHook = Arc::new(move |server_id| {
                    let db = db.clone();
                    let bus = bus.clone();
                    tokio::spawn(async move {
                        if let Err(error) = db
                            .call(move |conn| store::delete_account(conn, server_id))
                            .await
                        {
                            tracing::error!(%error, "cannot forget the signed-out account");
                        }
                        bus.publish(AppEvent::ServersChanged(ServersChanged {}));
                    });
                });
                let session = Arc::new(Session::new(row.id, base, self.vault.clone(), hook));
                sessions.insert(row.id, Arc::clone(&session));
                session
            }
        };
        session.set_blocked(row.blocked_fingerprint.is_some());
        Ok(session)
    }

    /// The API client for a server (requests fail while it is blocked).
    pub async fn api(&self, id: Uuid) -> Result<ApiClient, AppError> {
        let row = self
            .row(id)
            .await
            .map_err(|e| AppError::internal("load the server", &e))?
            .ok_or(AppError::NotFound)?;
        self.client_for(&row)
            .map_err(|e| AppError::internal("load the server", &e))
    }

    fn client_for(&self, row: &ServerRow) -> Result<ApiClient, DbError> {
        let session = self.session_for(row)?;
        let base = Url::parse(&row.url).map_err(|_| DbError::Closed)?;
        Ok(ApiClient::new(self.http.clone(), base, session))
    }

    /// Checks the server's identity against the pin, then refreshes its trust
    /// bundle. Run at startup and after every switch.
    pub async fn connect(&self, id: Uuid) -> Result<(), ConnectError> {
        let row = self.row(id).await?.ok_or(ConnectError::NotFound)?;
        let session = self.session_for(&row)?;
        let base = Url::parse(&row.url).map_err(|_| ConnectError::NotFound)?;
        let discovered = match discover(&self.http, &base, &self.config.launcher_version).await {
            Ok(d) => d,
            Err(error) => {
                self.publish_connectivity(id, false);
                return Err(ConnectError::Offline(error));
            }
        };
        let presented = discovered.root;
        let known = Self::pin_of(&row)
            .is_some_and(|pin| presented == pin.root || pin.next_root == Some(presented));
        if !known {
            let presented = discovered.fingerprint.to_string();
            tracing::error!(
                server_id = %id,
                pinned = %row.root_fingerprint,
                presented = %presented,
                "server root key does not match the pin; blocking"
            );
            let text = presented.clone();
            self.db
                .call(move |conn| store::set_blocked(conn, id, Some(&text)))
                .await?;
            session.set_blocked(true);
            if let Ok(mut trust) = self.trust.lock() {
                trust.remove(&id);
            }
            self.bus.publish(AppEvent::TrustProblem(TrustProblem {
                server_id: id,
                kind: TrustProblemKind::FingerprintMismatch,
                server_name: row.name.clone(),
                pinned_fingerprint: row.root_fingerprint.clone(),
                presented_fingerprint: presented.clone(),
            }));
            self.bus
                .publish(AppEvent::ServersChanged(ServersChanged {}));
            return Err(ConnectError::Blocked { presented });
        }
        if discovered.server_id != id {
            tracing::warn!(server_id = %id, presented = %discovered.server_id, "server id changed with the same root key");
        }
        let name = discovered.name.clone();
        let was_blocked = row.blocked_fingerprint.is_some();
        self.db
            .call(move |conn| {
                if was_blocked {
                    store::set_blocked(conn, id, None)?;
                }
                store::touch_connected(conn, id, &name)
            })
            .await?;
        if was_blocked {
            tracing::info!(server_id = %id, "server presents the pinned root again; unblocked");
            session.set_blocked(false);
            self.bus
                .publish(AppEvent::ServersChanged(ServersChanged {}));
        }
        self.publish_connectivity(id, true);
        if let Err(error) = self.refresh_trust(id).await {
            tracing::warn!(server_id = %id, %error, "trust bundle refresh failed");
        }
        Ok(())
    }

    fn publish_connectivity(&self, id: Uuid, online: bool) {
        self.bus
            .publish(AppEvent::ConnectivityChanged(ConnectivityChanged {
                server_id: id,
                online,
            }));
    }

    /// The stored bundle, re-verified against the stored root (it was stored
    /// only after verification; this also recovers the announced next root).
    fn stored_bundle(row: &ServerRow) -> Option<VerifiedBundle> {
        let bytes = row.trust_bundle.as_ref()?;
        let signature: [u8; 64] = row.trust_signature.as_deref()?.try_into().ok()?;
        let root = PublicKey::from_bytes(&row.root_public_key).ok()?;
        match verify_bundle(
            bytes,
            &Signature::from_bytes(signature),
            &RootPin::new(root),
            None,
            row.id,
        ) {
            Ok(verified) => Some(verified),
            Err(error) => {
                tracing::error!(server_id = %row.id, %error, "stored trust bundle no longer verifies");
                None
            }
        }
    }

    /// The pinned root and any announced next root. `None` only for a row whose
    /// stored key is unusable (keys are validated before insert).
    fn pin_of(row: &ServerRow) -> Option<RootPin> {
        match Self::stored_bundle(row) {
            Some(verified) => Some(verified.pin),
            None => PublicKey::from_bytes(&row.root_public_key)
                .ok()
                .map(RootPin::new),
        }
    }

    /// The server's current trust state (memory, else the stored bundle).
    /// `None` until a bundle was verified. Does not contact the server.
    pub async fn trust_state(&self, id: Uuid) -> Result<Option<Arc<TrustState>>, DbError> {
        if let Some(state) = self.trust.lock().ok().and_then(|t| t.get(&id).cloned()) {
            return Ok(Some(state));
        }
        let Some(row) = self.row(id).await? else {
            return Ok(None);
        };
        if row.blocked_fingerprint.is_some() {
            return Ok(None);
        }
        let state = Self::stored_bundle(&row).map(|v| Arc::new(v.state));
        if let (Some(state), Ok(mut trust)) = (&state, self.trust.lock()) {
            trust.insert(id, Arc::clone(state));
        }
        Ok(state)
    }

    /// Fetches, verifies and stores the server's trust bundle. Call it on
    /// startup, on switch, and when the API reports an unknown signing key.
    /// Returns `None` when the server has not published a bundle yet.
    pub async fn refresh_trust(
        &self,
        id: Uuid,
    ) -> Result<Option<Arc<TrustState>>, TrustRefreshError> {
        let row = self.row(id).await?.ok_or(TrustRefreshError::NotFound)?;
        let client = self.client_for(&row)?;
        let signed: SignedBundle = match client
            .public(Method::GET, "v1/trust/bundle", None::<&()>)
            .await
        {
            Ok(signed) => signed,
            Err(ApiError::Problem { status: 404, .. }) => {
                return Ok(self.trust_state(id).await?);
            }
            Err(error) => return Err(error.into()),
        };
        let bytes = signed.bundle_bytes()?;
        let pin = Self::pin_of(&row).ok_or(TrustError::Signature)?;
        let verified = match verify_bundle(&bytes, &signed.signature, &pin, row.trust_version, id) {
            Ok(verified) => verified,
            Err(error) => {
                tracing::warn!(server_id = %id, %error, "trust bundle refused");
                return Err(error.into());
            }
        };
        if verified.rotated {
            tracing::info!(
                server_id = %id,
                fingerprint = %verified.pin.root.fingerprint(),
                "root key rotation completed; pin moved"
            );
        }
        let version = verified.state.version();
        let root = *verified.pin.root.as_bytes();
        let fingerprint = verified.pin.root.fingerprint().to_string();
        let signature = *signed.signature.as_bytes();
        let stored = self
            .db
            .call(move |conn| {
                store::save_trust(
                    conn,
                    id,
                    &store::TrustUpdate {
                        version,
                        bundle: &bytes,
                        signature: &signature,
                        root_public_key: &root,
                        root_fingerprint: &fingerprint,
                    },
                )
            })
            .await?;
        if !stored {
            // A newer bundle was stored concurrently: use that one.
            if let Ok(mut trust) = self.trust.lock() {
                trust.remove(&id);
            }
            return Ok(self.trust_state(id).await?);
        }
        let state = Arc::new(verified.state);
        if let Ok(mut trust) = self.trust.lock() {
            trust.insert(id, Arc::clone(&state));
        }
        if verified.rotated {
            self.bus
                .publish(AppEvent::ServersChanged(ServersChanged {}));
        }
        Ok(Some(state))
    }
}

fn poisoned() -> ServerError {
    ServerError::Unreachable {
        detail: "internal state is unavailable".into(),
    }
}

#[cfg(test)]
mod tests;
