//! One server's sign-in session (01-security §4.2).
//!
//! The access and refresh tokens are kept together in the token vault (OS
//! keychain) under `tokens:<server_id>`, and in memory once loaded. Refresh is
//! single-flight: concurrent callers that saw the same token generation wait
//! for one `POST /v1/auth/token` and all use its result. A refresh is never
//! retried with the same token: the server rotates on every use and treats a
//! rotated-out token as theft (`refresh_token_reused` revokes the session).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use url::Url;
use uuid::Uuid;
use vgames_proto::auth::{TokenRequest, TokenResponse};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::{ApiError, json_or_problem};
use crate::secrets::VaultHandle;

/// Refresh this long before the access token expires.
const EXPIRY_MARGIN: Duration = Duration::from_secs(30);

/// A usable access token and the generation it belongs to.
pub struct Grant {
    pub token: Zeroizing<String>,
    pub generation: u64,
}

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct StoredTokens {
    access: String,
    refresh: String,
}

#[derive(Default)]
struct TokenState {
    loaded: bool,
    tokens: Option<StoredTokens>,
    /// Known only for tokens received in this process.
    access_expires: Option<Instant>,
    /// Bumped on every change, so waiters can tell a refresh already happened.
    generation: u64,
}

/// Called (from the refreshing task) when the server ended the session.
pub type SignedOutHook = Arc<dyn Fn(Uuid) + Send + Sync>;

pub struct Session {
    server_id: Uuid,
    base: Url,
    vault: VaultHandle,
    state: Mutex<TokenState>,
    blocked: AtomicBool,
    on_signed_out: SignedOutHook,
}

impl Session {
    pub fn new(
        server_id: Uuid,
        base: Url,
        vault: VaultHandle,
        on_signed_out: SignedOutHook,
    ) -> Self {
        Self {
            server_id,
            base,
            vault,
            state: Mutex::new(TokenState::default()),
            blocked: AtomicBool::new(false),
            on_signed_out,
        }
    }

    pub fn server_id(&self) -> Uuid {
        self.server_id
    }

    fn vault_key(&self) -> String {
        format!("tokens:{}", self.server_id)
    }

    /// Blocks (or unblocks) every request to this server.
    pub fn set_blocked(&self, blocked: bool) {
        self.blocked.store(blocked, Ordering::SeqCst);
    }

    pub fn is_blocked(&self) -> bool {
        self.blocked.load(Ordering::SeqCst)
    }

    pub fn check_trusted(&self) -> Result<(), ApiError> {
        if self.is_blocked() {
            Err(ApiError::TrustBlocked)
        } else {
            Ok(())
        }
    }

    async fn load(&self, state: &mut TokenState) {
        if state.loaded {
            return;
        }
        match self.vault.get(self.vault_key()).await {
            Ok(Some(text)) => match serde_json::from_str::<StoredTokens>(&text) {
                Ok(tokens) => state.tokens = Some(tokens),
                Err(_) => {
                    tracing::warn!(server_id = %self.server_id, "stored tokens are malformed; sign-in required")
                }
            },
            Ok(None) => {}
            Err(error) => {
                // Not cached as loaded: the keychain may be locked right now.
                tracing::warn!(%error, server_id = %self.server_id, "cannot read stored tokens");
                return;
            }
        }
        state.loaded = true;
    }

    /// Whether tokens are stored (does not contact the server).
    pub async fn has_tokens(&self) -> bool {
        let mut state = self.state.lock().await;
        self.load(&mut state).await;
        state.tokens.is_some()
    }

    /// The current access token, refreshed first when it is about to expire.
    pub async fn access_token(&self, http: &reqwest::Client) -> Result<Grant, ApiError> {
        let mut state = self.state.lock().await;
        self.load(&mut state).await;
        let expiring = state
            .access_expires
            .is_some_and(|at| at <= Instant::now() + EXPIRY_MARGIN);
        if expiring {
            self.refresh_locked(http, &mut state).await?;
        }
        Self::grant(&state)
    }

    /// Refreshes unless another caller already did since `seen_generation`.
    pub async fn refresh(
        &self,
        http: &reqwest::Client,
        seen_generation: u64,
    ) -> Result<Grant, ApiError> {
        let mut state = self.state.lock().await;
        self.load(&mut state).await;
        if state.generation == seen_generation {
            self.refresh_locked(http, &mut state).await?;
        }
        Self::grant(&state)
    }

    fn grant(state: &TokenState) -> Result<Grant, ApiError> {
        let tokens = state.tokens.as_ref().ok_or(ApiError::Unauthenticated)?;
        Ok(Grant {
            token: Zeroizing::new(tokens.access.clone()),
            generation: state.generation,
        })
    }

    async fn refresh_locked(
        &self,
        http: &reqwest::Client,
        state: &mut TokenState,
    ) -> Result<(), ApiError> {
        self.check_trusted()?;
        let refresh_token = match &state.tokens {
            Some(tokens) => tokens.refresh.clone(),
            None => return Err(ApiError::Unauthenticated),
        };
        let url = self
            .base
            .join("v1/auth/token")
            .map_err(|e| ApiError::InvalidResponse(e.to_string()))?;
        let request = TokenRequest::RefreshToken { refresh_token };
        // Never retried: see the module docs.
        let response = http
            .post(url)
            .json(&request)
            .send()
            .await
            .map_err(|e| ApiError::from_reqwest(&e));
        drop(request);
        let response = response?;
        let status = response.status();
        match json_or_problem::<TokenResponse>(response).await {
            Ok(tokens) => {
                self.store_locked(state, tokens).await;
                tracing::debug!(server_id = %self.server_id, "session refreshed");
                Ok(())
            }
            Err(error) if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN => {
                tracing::info!(server_id = %self.server_id, %error, "the server ended the session");
                self.clear_locked(state).await;
                (self.on_signed_out)(self.server_id);
                Err(ApiError::Unauthenticated)
            }
            Err(error) => Err(error),
        }
    }

    /// Stores a fresh token pair (sign-in or refresh).
    pub async fn store(&self, tokens: &TokenResponse) {
        let mut state = self.state.lock().await;
        self.store_locked(&mut state, tokens.clone()).await;
    }

    async fn store_locked(&self, state: &mut TokenState, response: TokenResponse) {
        let tokens = StoredTokens {
            access: response.access_token.clone(),
            refresh: response.refresh_token.clone(),
        };
        let mut response = response;
        response.access_token.zeroize();
        response.refresh_token.zeroize();
        match serde_json::to_string(&tokens) {
            Ok(text) => {
                if let Err(error) = self.vault.set(self.vault_key(), Zeroizing::new(text)).await {
                    // The old refresh token is already rotated out server-side, so
                    // keep the new pair in memory for this run.
                    tracing::error!(%error, server_id = %self.server_id, "cannot save the session tokens");
                }
            }
            Err(error) => tracing::error!(%error, "cannot encode the session tokens"),
        }
        let ttl = Duration::from_secs(u64::try_from(response.expires_in).unwrap_or(0));
        state.access_expires = Some(Instant::now() + ttl);
        state.tokens = Some(tokens);
        state.loaded = true;
        state.generation += 1;
    }

    /// Forgets the tokens (memory and vault).
    pub async fn clear(&self) {
        let mut state = self.state.lock().await;
        self.clear_locked(&mut state).await;
    }

    async fn clear_locked(&self, state: &mut TokenState) {
        state.tokens = None;
        state.access_expires = None;
        state.loaded = true;
        state.generation += 1;
        if let Err(error) = self.vault.delete(self.vault_key()).await {
            tracing::error!(%error, server_id = %self.server_id, "cannot delete the stored tokens");
        }
    }
}
