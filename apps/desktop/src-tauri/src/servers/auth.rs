//! Discord sign-in (01-security §4.1): PKCE (S256) with a random
//! `client_state`, the system browser, then either the
//! `vgames://auth/callback?code=…&client_state=…` deep link or the code pasted
//! from the callback page. Both paths need a matching pending flow and the
//! verifier that only this process holds.

use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use specta::Type;
use url::Url;
use uuid::Uuid;
use vgames_proto::auth::{
    AuthStartRequest, AuthStartResponse, ClientKind, TokenRequest, TokenResponse,
};
use zeroize::Zeroizing;

use super::{Account, Servers, store};
use crate::api::ApiError;
use crate::error::AppError;
use crate::events::{AppEvent, AuthFinished, ServersChanged};

/// Upper bound on a flow's life whatever the server says (its flows last 10 min).
const MAX_FLOW_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_CODE_CHARS: usize = 256;

/// A started sign-in, as the UI sees it.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct AuthFlow {
    pub flow_id: Uuid,
    /// RFC 3339.
    pub expires_at: String,
    /// False when the system browser could not be opened; the UI then offers
    /// the paste-code fallback.
    pub browser_opened: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthError {
    #[error("this server does not accept new accounts")]
    RegistrationClosed,
    #[error("this Discord account is not on the server's allowlist")]
    NotAllowlisted,
    #[error("this account is disabled")]
    UserDisabled,
    #[error("the sign-in expired; start again")]
    Expired,
    #[error("the sign-in code is not valid")]
    InvalidCode,
    #[error("sign-in was cancelled")]
    Cancelled,
    #[error("the system browser could not be opened")]
    BrowserUnavailable,
    #[error("the server could not be reached: {detail}")]
    Network { detail: String },
    #[error("{message}")]
    Server { code: String, message: String },
}

impl AuthError {
    /// The `error` code of a refused-sign-in redirect (01-security §4.1).
    pub fn from_redirect(code: &str) -> Self {
        match code {
            "registration_closed" => Self::RegistrationClosed,
            "not_allowlisted" => Self::NotAllowlisted,
            "user_disabled" => Self::UserDisabled,
            "access_denied" => Self::Cancelled,
            _ => Self::Server {
                code: code.to_owned(),
                message: "The server could not complete the sign-in.".into(),
            },
        }
    }

    fn from_api(error: ApiError) -> Self {
        match error {
            ApiError::Timeout => Self::Network {
                detail: "the server did not answer in time".into(),
            },
            ApiError::Tls(detail) | ApiError::Network(detail) => Self::Network { detail },
            ApiError::Problem { code, message, .. } => match code.as_str() {
                "invalid_grant" | "invalid_code" => Self::InvalidCode,
                "registration_closed" => Self::RegistrationClosed,
                "not_allowlisted" => Self::NotAllowlisted,
                "user_disabled" => Self::UserDisabled,
                "invalid_state" => Self::Expired,
                _ => Self::Server { code, message },
            },
            ApiError::TrustBlocked => Self::Server {
                code: "trust_blocked".into(),
                message: "This server's identity changed; it is blocked.".into(),
            },
            ApiError::Unauthenticated => Self::InvalidCode,
            ApiError::InvalidResponse(detail) => Self::Server {
                code: "invalid_response".into(),
                message: detail,
            },
        }
    }
}

/// How a sign-in ended (the `AuthFinished` event).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthOutcome {
    SignedIn { account: Account },
    Failed { error: AuthError },
}

pub(super) struct PendingFlow {
    pub(super) server_id: Uuid,
    verifier: Zeroizing<String>,
    client_state: String,
    authorize_url: Url,
    expires: Instant,
}

fn random_b64url(bytes: usize) -> Result<String, AuthError> {
    let mut buf = Zeroizing::new(vec![0u8; bytes]);
    getrandom::fill(&mut buf).map_err(|_| AuthError::Server {
        code: "internal".into(),
        message: "the OS random generator failed".into(),
    })?;
    Ok(URL_SAFE_NO_PAD.encode(&*buf))
}

/// `BASE64URL(SHA-256(verifier))` without padding (RFC 7636 S256).
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn valid_token_chars(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Servers {
    /// Starts a sign-in on `server_id` and opens the system browser.
    pub async fn auth_start(&self, server_id: Uuid) -> Result<AuthFlow, AuthError> {
        let client = self.api(server_id).await.map_err(|e| match e {
            AppError::NotFound => AuthError::Server {
                code: "not_found".into(),
                message: "This server is not added.".into(),
            },
            other => AuthError::Server {
                code: "internal".into(),
                message: other.to_string(),
            },
        })?;
        let verifier = Zeroizing::new(random_b64url(32)?);
        let client_state = random_b64url(16)?;
        let request = AuthStartRequest {
            client: ClientKind::Desktop,
            code_challenge: Some(pkce_challenge(&verifier)),
            client_state: Some(client_state.clone()),
            device_name: Some(self.config.device_name.clone()),
            return_to: None,
        };
        let response: AuthStartResponse = client
            .public(Method::POST, "v1/auth/discord/start", Some(&request))
            .await
            .map_err(AuthError::from_api)?;
        let authorize_url = self.check_authorize_url(&response.authorize_url)?;

        let now = time::OffsetDateTime::now_utc();
        let ttl = (response.expires_at - now)
            .try_into()
            .unwrap_or(Duration::ZERO)
            .min(MAX_FLOW_TTL);
        let expires_at = now + ttl;
        let flow_id = Uuid::now_v7();
        {
            let mut flows = self.flows.lock().map_err(|_| internal_state())?;
            // One sign-in per server at a time; stale flows go.
            flows.retain(|_, f| f.server_id != server_id && f.expires > Instant::now());
            flows.insert(
                flow_id,
                PendingFlow {
                    server_id,
                    verifier,
                    client_state,
                    authorize_url: authorize_url.clone(),
                    expires: Instant::now() + ttl,
                },
            );
        }
        let browser_opened = self.config.browser.open(&authorize_url);
        tracing::info!(%server_id, %flow_id, browser_opened, "sign-in started");
        Ok(AuthFlow {
            flow_id,
            expires_at: expires_at
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
            browser_opened,
        })
    }

    /// The authorize URL is opened in the browser, so only web URLs pass.
    fn check_authorize_url(&self, text: &str) -> Result<Url, AuthError> {
        let url = Url::parse(text).ok().filter(|url| match url.scheme() {
            "https" => true,
            "http" => {
                self.config.allow_loopback_http
                    && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
            }
            _ => false,
        });
        url.ok_or_else(|| AuthError::Server {
            code: "invalid_response".into(),
            message: "The server sent an invalid sign-in address.".into(),
        })
    }

    /// Opens the browser again for a pending flow.
    pub fn auth_open_browser(&self, flow_id: Uuid) -> Result<(), AuthError> {
        let url = {
            let flows = self.flows.lock().map_err(|_| internal_state())?;
            let flow = flows.get(&flow_id).ok_or(AuthError::Expired)?;
            if flow.expires <= Instant::now() {
                return Err(AuthError::Expired);
            }
            flow.authorize_url.clone()
        };
        if self.config.browser.open(&url) {
            Ok(())
        } else {
            Err(AuthError::BrowserUnavailable)
        }
    }

    /// The paste-code fallback. Also accepts the whole `vgames://auth/callback`
    /// link, whose `client_state` must then match the flow.
    pub async fn auth_submit_code(&self, flow_id: Uuid, input: &str) -> Result<Account, AuthError> {
        let result = self.submit_code(flow_id, input).await;
        self.publish_finished(flow_id, &result);
        result
    }

    async fn submit_code(&self, flow_id: Uuid, input: &str) -> Result<Account, AuthError> {
        let input = input.trim();
        let (code, state) = match parse_callback_link(input) {
            Some((code, state)) => (code, Some(state)),
            None => (input.to_owned(), None),
        };
        if code.is_empty() || code.len() > MAX_CODE_CHARS || !valid_token_chars(&code) {
            return Err(AuthError::InvalidCode);
        }
        let flow = self.take_flow(flow_id)?;
        if let Some(state) = state
            && !constant_time_eq(&state, &flow.client_state)
        {
            return Err(AuthError::InvalidCode);
        }
        self.exchange(flow, code).await
    }

    /// The `vgames://auth/callback` deep link. Ignored unless a pending flow
    /// has this `client_state`.
    pub async fn auth_callback(&self, code: &str, client_state: &str) {
        let found = self.flows.lock().ok().and_then(|flows| {
            flows
                .iter()
                .find(|(_, f)| constant_time_eq(&f.client_state, client_state))
                .map(|(id, _)| *id)
        });
        let Some(flow_id) = found else {
            tracing::warn!("sign-in callback without a matching pending sign-in; ignored");
            return;
        };
        let result = if code.is_empty() || code.len() > MAX_CODE_CHARS || !valid_token_chars(code) {
            Err(AuthError::InvalidCode)
        } else {
            match self.take_flow(flow_id) {
                Ok(flow) => self.exchange(flow, code.to_owned()).await,
                Err(error) => Err(error),
            }
        };
        self.publish_finished(flow_id, &result);
    }

    /// The refused-sign-in deep link: ends the pending flow with this
    /// `client_state` and reports why. Ignored without a matching flow.
    pub fn auth_callback_error(&self, error: &str, client_state: &str) {
        let removed = self.flows.lock().ok().and_then(|mut flows| {
            let id = flows
                .iter()
                .find(|(_, f)| constant_time_eq(&f.client_state, client_state))
                .map(|(id, _)| *id)?;
            flows.remove(&id).map(|_| id)
        });
        let Some(flow_id) = removed else {
            tracing::warn!("refused sign-in callback without a matching pending sign-in; ignored");
            return;
        };
        self.publish_finished(flow_id, &Err(AuthError::from_redirect(error)));
    }

    /// Cancels a pending flow (unknown ids are ignored).
    pub fn auth_cancel(&self, flow_id: Uuid) {
        let removed = self
            .flows
            .lock()
            .ok()
            .and_then(|mut flows| flows.remove(&flow_id));
        if removed.is_some() {
            self.publish_finished(flow_id, &Err(AuthError::Cancelled));
        }
    }

    fn take_flow(&self, flow_id: Uuid) -> Result<PendingFlow, AuthError> {
        let flow = self
            .flows
            .lock()
            .map_err(|_| internal_state())?
            .remove(&flow_id)
            .ok_or(AuthError::Expired)?;
        if flow.expires <= Instant::now() {
            return Err(AuthError::Expired);
        }
        Ok(flow)
    }

    async fn exchange(&self, flow: PendingFlow, code: String) -> Result<Account, AuthError> {
        let server_id = flow.server_id;
        let client = self.api(server_id).await.map_err(|_| AuthError::Expired)?;
        let request = TokenRequest::AuthorizationCode {
            code,
            code_verifier: flow.verifier.to_string(),
        };
        let tokens: TokenResponse = client
            .public(Method::POST, "v1/auth/token", Some(&request))
            .await
            .map_err(AuthError::from_api)?;
        drop(request);
        let account = Account::from(&tokens.user);
        client.session().store(&tokens).await;
        drop(tokens);
        let stored = account.clone();
        self.db
            .call(move |conn| store::upsert_account(conn, server_id, &stored))
            .await
            .map_err(|e| {
                tracing::error!(error = %crate::error::DisplayChain(&e), "cannot save the account");
                AuthError::Server {
                    code: "internal".into(),
                    message: "The local database failed. See the log for details.".into(),
                }
            })?;
        tracing::info!(%server_id, "signed in");
        self.bus
            .publish(AppEvent::ServersChanged(ServersChanged {}));
        Ok(account)
    }

    fn publish_finished(&self, flow_id: Uuid, result: &Result<Account, AuthError>) {
        let outcome = match result {
            Ok(account) => AuthOutcome::SignedIn {
                account: account.clone(),
            },
            Err(error) => AuthOutcome::Failed {
                error: error.clone(),
            },
        };
        self.bus
            .publish(AppEvent::AuthFinished(AuthFinished { flow_id, outcome }));
    }

    /// Revokes this device's session on the server (best effort) and forgets
    /// the tokens and the account locally.
    pub async fn sign_out(&self, server_id: Uuid) -> Result<(), AppError> {
        let client = self.api(server_id).await?;
        let session = client.session();
        if session.has_tokens().await
            && !session.is_blocked()
            && let Err(error) = client
                .authed_empty(Method::POST, "v1/auth/logout", None::<&()>)
                .await
        {
            tracing::info!(%server_id, %error, "server-side sign-out failed; forgetting the tokens anyway");
        }
        self.forget_account(server_id).await
    }

    /// Forgets this launcher's session on `server_id` (tokens and account) without telling the
    /// server, for a session the server already ended.
    pub async fn forget_account(&self, server_id: Uuid) -> Result<(), AppError> {
        let client = self.api(server_id).await?;
        client.session().clear().await;
        self.db
            .call(move |conn| store::delete_account(conn, server_id))
            .await
            .map_err(|e| AppError::internal("sign out", &e))?;
        self.bus
            .publish(AppEvent::ServersChanged(ServersChanged {}));
        Ok(())
    }
}

/// `vgames://auth/callback?code=…&client_state=…` → `(code, client_state)`.
pub fn parse_callback_link(text: &str) -> Option<(String, String)> {
    match callback_params(text)? {
        (Some(code), None, state) => Some((code, state)),
        _ => None,
    }
}

/// A refused sign-in (01-security §4.1):
/// `vgames://auth/callback?error=<code>&client_state=…` → `(error, client_state)`.
/// The error code must be `^[a-z_]{1,64}$`.
pub fn parse_callback_error_link(text: &str) -> Option<(String, String)> {
    match callback_params(text)? {
        (None, Some(error), state)
            if (1..=64).contains(&error.len())
                && error.bytes().all(|c| c.is_ascii_lowercase() || c == b'_') =>
        {
            Some((error, state))
        }
        _ => None,
    }
}

/// `(code, error, client_state)` of a callback link with no unknown or repeated keys.
fn callback_params(text: &str) -> Option<(Option<String>, Option<String>, String)> {
    let url = Url::parse(text).ok()?;
    if url.scheme() != "vgames" || url.host_str() != Some("auth") || url.path() != "/callback" {
        return None;
    }
    let mut code = None;
    let mut error = None;
    let mut state = None;
    for (key, value) in url.query_pairs() {
        let slot = match key.as_ref() {
            "code" => &mut code,
            "error" => &mut error,
            "client_state" | "state" => &mut state,
            _ => return None,
        };
        if slot.replace(value.into_owned()).is_some() {
            return None;
        }
    }
    Some((code, error, state?))
}

fn internal_state() -> AuthError {
    AuthError::Server {
        code: "internal".into(),
        message: "internal state is unavailable".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc_7636_appendix_b() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn random_values_have_the_expected_shape() {
        let verifier = random_b64url(32).unwrap();
        assert_eq!(verifier.len(), 43);
        assert!(valid_token_chars(&verifier));
        let state = random_b64url(16).unwrap();
        assert_eq!(state.len(), 22);
        assert_ne!(random_b64url(16).unwrap(), state);
    }

    #[test]
    fn callback_links_are_parsed_strictly() {
        assert_eq!(
            parse_callback_link("vgames://auth/callback?code=abc&client_state=xyz"),
            Some(("abc".into(), "xyz".into()))
        );
        for bad in [
            "vgames://auth/callback?code=abc",
            "vgames://auth/callback?code=a&code=b&client_state=x",
            "vgames://auth/callback?code=a&client_state=x&extra=1",
            "vgames://auth/other?code=a&client_state=x",
            "https://auth/callback?code=a&client_state=x",
            "abc",
        ] {
            assert_eq!(parse_callback_link(bad), None, "{bad}");
        }
    }

    #[test]
    fn refused_sign_in_links_are_parsed_strictly() {
        assert_eq!(
            parse_callback_error_link(
                "vgames://auth/callback?error=not_allowlisted&client_state=xyz"
            ),
            Some(("not_allowlisted".into(), "xyz".into()))
        );
        for bad in [
            "vgames://auth/callback?error=not_allowlisted",
            "vgames://auth/callback?error=x&code=a&client_state=s",
            "vgames://auth/callback?error=x&error=y&client_state=s",
            "vgames://auth/callback?error=Bad-Code&client_state=s",
            "vgames://auth/callback?error=&client_state=s",
            "vgames://auth/callback?error=x&client_state=s&extra=1",
        ] {
            assert_eq!(parse_callback_error_link(bad), None, "{bad}");
        }
        // A code link is not an error link and vice versa.
        assert_eq!(
            parse_callback_error_link("vgames://auth/callback?code=a&client_state=s"),
            None
        );
        assert_eq!(
            parse_callback_link("vgames://auth/callback?error=x&client_state=s"),
            None
        );
    }

    #[test]
    fn redirect_codes_map_to_auth_errors() {
        assert_eq!(
            AuthError::from_redirect("registration_closed"),
            AuthError::RegistrationClosed
        );
        assert_eq!(
            AuthError::from_redirect("not_allowlisted"),
            AuthError::NotAllowlisted
        );
        assert_eq!(
            AuthError::from_redirect("user_disabled"),
            AuthError::UserDisabled
        );
        assert_eq!(
            AuthError::from_redirect("access_denied"),
            AuthError::Cancelled
        );
        assert!(matches!(
            AuthError::from_redirect("sign_in_failed"),
            AuthError::Server { code, .. } if code == "sign_in_failed"
        ));
    }

    #[test]
    fn comparison_is_exact() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
    }
}
