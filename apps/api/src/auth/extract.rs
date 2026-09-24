//! Resolving the caller: `CurrentUser`, role guards and request metadata.

use std::net::IpAddr;

use axum::{
    extract::FromRequestParts,
    http::{Method, header, request::Parts},
};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;
use vgames_proto::auth::{ClientKind, Role};

use super::tokens::{self, ACCESS_PREFIX, WEB_SESSION_PREFIX};
use crate::{
    error::ApiError,
    http::{client_ip::client_ip, ratelimit::Policy},
    state::AppState,
};

pub const SESSION_COOKIE: &str = "__Host-vgames_session";
pub const CSRF_COOKIE: &str = "__Host-vgames_csrf";
pub const CSRF_HEADER: &str = "x-csrf-token";

/// Web sessions slide by this much on use, up to [`WEB_ABSOLUTE`] after sign-in.
pub const WEB_IDLE: Duration = Duration::hours(12);
pub const WEB_ABSOLUTE: Duration = Duration::days(7);

/// The authenticated caller.
#[derive(Clone, Debug)]
pub struct CurrentUser {
    pub user_id: Uuid,
    pub role: Role,
    pub session_id: Uuid,
    pub device_id: Option<Uuid>,
    pub kind: ClientKind,
}

impl CurrentUser {
    pub fn is_admin(&self) -> bool {
        self.role >= Role::Admin
    }
}

pub fn parse_role(s: &str) -> Role {
    match s {
        "owner" => Role::Owner,
        "admin" => Role::Admin,
        _ => Role::User,
    }
}

pub fn role_str(r: Role) -> &'static str {
    match r {
        Role::Owner => "owner",
        Role::Admin => "admin",
        Role::User => "user",
    }
}

/// Reads `name` from the `Cookie` header(s).
pub fn cookie(parts: &Parts, name: &str) -> Option<String> {
    parts
        .headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

enum Credential {
    Bearer(String),
    Cookie(String),
}

fn credential(parts: &Parts) -> Result<Credential, ApiError> {
    if let Some(v) = parts.headers.get(header::AUTHORIZATION) {
        let v = v.to_str().map_err(|_| ApiError::unauthenticated())?;
        let token = v
            .strip_prefix("Bearer ")
            .or_else(|| v.strip_prefix("bearer "))
            .ok_or_else(ApiError::unauthenticated)?
            .trim();
        if !tokens::well_formed(token, ACCESS_PREFIX) {
            return Err(ApiError::unauthenticated());
        }
        return Ok(Credential::Bearer(token.to_string()));
    }
    match cookie(parts, SESSION_COOKIE) {
        Some(t) if tokens::well_formed(&t, WEB_SESSION_PREFIX) => Ok(Credential::Cookie(t)),
        _ => Err(ApiError::unauthenticated()),
    }
}

fn is_unsafe(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(user) = parts.extensions.get::<CurrentUser>() {
            return Ok(user.clone());
        }
        let cred = credential(parts)?;
        let (token, via_cookie) = match &cred {
            Credential::Bearer(t) => (t.as_str(), false),
            Credential::Cookie(t) => (t.as_str(), true),
        };
        let row = sqlx::query!(
            r#"SELECT s.id AS session_id, s.kind, s.device_id, s.access_expires_at, s.csrf_token_hash,
                      s.created_at, s.last_used_at, u.id AS user_id, u.role, u.disabled_at
               FROM sessions s JOIN users u ON u.id = s.user_id
               WHERE s.access_token_hash = $1 AND s.revoked_at IS NULL"#,
            tokens::digest(token)
        )
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(ApiError::unauthenticated)?;

        let kind = if row.kind == "web" {
            ClientKind::Web
        } else {
            ClientKind::Desktop
        };
        if via_cookie != (kind == ClientKind::Web) {
            return Err(ApiError::unauthenticated());
        }
        let now = OffsetDateTime::now_utc();
        if row.access_expires_at <= now
            || (kind == ClientKind::Web && row.created_at + WEB_ABSOLUTE <= now)
        {
            return Err(ApiError::session_expired());
        }
        if row.disabled_at.is_some() {
            revoke_all_for_user(state, row.user_id, "user_disabled").await?;
            return Err(ApiError::forbidden_code(
                "user_disabled",
                "This account is disabled",
            ));
        }
        if via_cookie && is_unsafe(&parts.method) {
            check_csrf(parts, state, row.csrf_token_hash.as_deref())?;
        }
        state
            .limits
            .check(Policy::User, &format!("user:{}", row.user_id))?;

        if now - row.last_used_at > Duration::seconds(60) {
            // Slide web sessions; record activity at most once a minute.
            sqlx::query!(
                r#"UPDATE sessions
                   SET last_used_at = now(),
                       access_expires_at = CASE WHEN kind = 'web'
                           THEN LEAST(now() + make_interval(secs => $2), created_at + make_interval(secs => $3))
                           ELSE access_expires_at END
                   WHERE id = $1"#,
                row.session_id,
                WEB_IDLE.as_seconds_f64(),
                WEB_ABSOLUTE.as_seconds_f64()
            )
            .execute(&state.db)
            .await?;
        }

        let user = CurrentUser {
            user_id: row.user_id,
            role: parse_role(&row.role),
            session_id: row.session_id,
            device_id: row.device_id,
            kind,
        };
        parts.extensions.insert(user.clone());
        Ok(user)
    }
}

/// Cookie-authenticated unsafe requests need a matching `X-CSRF-Token` and a same-origin `Origin`.
fn check_csrf(
    parts: &Parts,
    state: &AppState,
    expected_hash: Option<&[u8]>,
) -> Result<(), ApiError> {
    let fail = || {
        ApiError::forbidden_code(
            "csrf_failed",
            "The request failed cross-site request forgery checks",
        )
    };
    let origin = parts
        .headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(fail)?;
    if origin != state.config.public_origin() {
        return Err(fail());
    }
    let token = parts
        .headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(fail)?;
    let expected = expected_hash.ok_or_else(fail)?;
    if !tokens::constant_time_eq(&tokens::digest(token), expected) {
        return Err(fail());
    }
    Ok(())
}

pub async fn revoke_all_for_user(
    state: &AppState,
    user_id: Uuid,
    reason: &str,
) -> Result<(), ApiError> {
    sqlx::query!(
        "UPDATE sessions SET revoked_at = now(), revoked_reason = $2 WHERE user_id = $1 AND revoked_at IS NULL",
        user_id,
        reason
    )
    .execute(&state.db)
    .await?;
    Ok(())
}

/// Requires `admin` or `owner`.
pub struct RequireAdmin(pub CurrentUser);

impl FromRequestParts<AppState> for RequireAdmin {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = CurrentUser::from_request_parts(parts, state).await?;
        if user.role >= Role::Admin {
            Ok(RequireAdmin(user))
        } else {
            Err(ApiError::forbidden())
        }
    }
}

/// Requires `owner`.
pub struct RequireOwner(pub CurrentUser);

impl FromRequestParts<AppState> for RequireOwner {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = CurrentUser::from_request_parts(parts, state).await?;
        if user.role == Role::Owner {
            Ok(RequireOwner(user))
        } else {
            Err(ApiError::forbidden())
        }
    }
}

/// Client IP and user agent, recorded on sessions and audit rows.
#[derive(Clone, Debug, Default)]
pub struct RequestMeta {
    pub ip: Option<IpAddr>,
    pub user_agent: Option<String>,
}

impl FromRequestParts<AppState> for RequestMeta {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(RequestMeta {
            ip: client_ip(parts, state.config.trust_proxy_headers),
            user_agent: parts
                .headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.chars().take(512).collect()),
        })
    }
}
