//! Discord sign-in, tokens and sessions (docs/architecture/01-security.md §4, A1-T04).

pub mod discord;
pub mod extract;
pub mod tokens;

use axum::{
    Router,
    extract::{Query as AxumQuery, State, rejection::QueryRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use serde::Deserialize;
use time::{Duration, OffsetDateTime};
use utoipa::IntoParams;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_proto::{
    FieldError,
    auth::{
        AuthStartRequest, AuthStartResponse, ClientKind, Me, Session, SessionList, TokenRequest,
        TokenResponse, User,
    },
    discovery::RegistrationMode,
};

use extract::{CSRF_COOKIE, SESSION_COOKIE, WEB_ABSOLUTE, WEB_IDLE, parse_role};
pub use extract::{CurrentUser, RequestMeta, RequireAdmin, RequireOwner};
use tokens::{ACCESS_PREFIX, REFRESH_PREFIX, WEB_SESSION_PREFIX};

use crate::http::path::Path;
use crate::{
    config::DiscordConfig,
    error::{ApiError, ApiResult},
    http::json::{Json, Validate, invalid},
    openapi_problems::{BadRequest, Forbidden, NotFound, TooManyRequests, Unauthorized},
    state::AppState,
};

const FLOW_TTL: Duration = Duration::minutes(10);
const LOGIN_CODE_TTL: Duration = Duration::seconds(60);
const ACCESS_TTL: Duration = Duration::minutes(15);
const REFRESH_TTL: Duration = Duration::days(30);
const REFRESH_ABSOLUTE: Duration = Duration::days(365);

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(start))
        .routes(routes!(callback))
        .routes(routes!(token))
        .routes(routes!(logout))
        .routes(routes!(me))
        .routes(routes!(list_sessions))
        .routes(routes!(revoke_session))
}

/// Debug-only fake identity provider pages (not part of the API contract).
pub fn dev_routes(state: &AppState) -> Router<AppState> {
    if cfg!(debug_assertions) && matches!(state.config.discord, DiscordConfig::Fake) {
        Router::new()
            .route("/v1/auth/dev/fake-discord", get(fake_page))
            .route("/v1/auth/dev/fake-discord/submit", get(fake_submit))
    } else {
        Router::new()
    }
}

// ---------------------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------------------

fn b64url_len(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_return_to(s: &str) -> bool {
    (s == "/admin" || s.starts_with("/admin/"))
        && s.len() <= 512
        && !s.contains("..")
        && !s.contains("//")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._~/-".contains(&b))
}

impl Validate for AuthStartRequest {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        match self.client {
            ClientKind::Desktop => {
                match &self.code_challenge {
                    None => invalid(
                        errors,
                        "code_challenge",
                        "required",
                        "is required for desktop sign-in",
                    ),
                    Some(c) if c.len() != 43 || !b64url_len(c, 43, 43) => invalid(
                        errors,
                        "code_challenge",
                        "invalid",
                        "must be 43 base64url characters (S256)",
                    ),
                    Some(_) => {}
                }
                if let Some(s) = &self.client_state
                    && !b64url_len(s, 16, 64)
                {
                    invalid(
                        errors,
                        "client_state",
                        "invalid",
                        "must be 16-64 base64url characters",
                    );
                }
                if self.return_to.is_some() {
                    invalid(
                        errors,
                        "return_to",
                        "not_allowed",
                        "is only for web sign-in",
                    );
                }
            }
            ClientKind::Web => {
                if self.code_challenge.is_some() {
                    invalid(
                        errors,
                        "code_challenge",
                        "not_allowed",
                        "is only for desktop sign-in",
                    );
                }
                if self.client_state.is_some() {
                    invalid(
                        errors,
                        "client_state",
                        "not_allowed",
                        "is only for desktop sign-in",
                    );
                }
                if let Some(r) = &self.return_to
                    && !valid_return_to(r)
                {
                    invalid(
                        errors,
                        "return_to",
                        "invalid",
                        "must be a path under /admin",
                    );
                }
            }
        }
        if let Some(d) = &self.device_name
            && (d.trim().is_empty() || d.chars().count() > 64)
        {
            invalid(errors, "device_name", "invalid", "must be 1-64 characters");
        }
    }
}

impl Validate for TokenRequest {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        match self {
            TokenRequest::AuthorizationCode {
                code,
                code_verifier,
            } => {
                if !b64url_len(code, 43, 64) {
                    invalid(errors, "code", "invalid", "is not a valid sign-in code");
                }
                let ok = (43..=128).contains(&code_verifier.len())
                    && code_verifier
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._~-".contains(&b));
                if !ok {
                    invalid(
                        errors,
                        "code_verifier",
                        "invalid",
                        "must be 43-128 unreserved characters (RFC 7636)",
                    );
                }
            }
            TokenRequest::RefreshToken { refresh_token } => {
                if !tokens::well_formed(refresh_token, REFRESH_PREFIX) {
                    invalid(errors, "refresh_token", "invalid", "is not a refresh token");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------------------

/// Begin Discord sign-in
#[utoipa::path(
    post,
    path = "/v1/auth/discord/start",
    tag = "auth",
    operation_id = "startDiscordLogin",
    security(()),
    request_body = AuthStartRequest,
    responses((status = 200, description = "URL to open in the system browser", body = AuthStartResponse), BadRequest, TooManyRequests)
)]
pub async fn start(
    State(state): State<AppState>,
    Json(req): Json<AuthStartRequest>,
) -> ApiResult<axum::Json<AuthStartResponse>> {
    let oauth_state = tokens::random_b64(32)?;
    let expires_at = OffsetDateTime::now_utc() + FLOW_TTL;
    let kind = match req.client {
        ClientKind::Desktop => "desktop",
        ClientKind::Web => "web",
    };
    let return_to = match req.client {
        ClientKind::Web => Some(
            req.return_to
                .clone()
                .unwrap_or_else(|| "/admin/".to_string()),
        ),
        ClientKind::Desktop => None,
    };
    sqlx::query!(
        r#"INSERT INTO oauth_flows (state_hash, client_kind, code_challenge, client_state, device_name, return_to, expires_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        tokens::digest(&oauth_state),
        kind,
        req.code_challenge,
        req.client_state,
        req.device_name.as_deref().map(str::trim),
        return_to,
        expires_at
    )
    .execute(&state.db)
    .await?;
    Ok(axum::Json(AuthStartResponse {
        authorize_url: discord::authorize_url(&state, &oauth_state)?,
        expires_at,
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CallbackParams {
    /// Discord authorization code.
    pub code: Option<String>,
    /// The OAuth state issued by `startDiscordLogin`.
    pub state: String,
    /// Set by Discord when the user declined.
    pub error: Option<String>,
}

/// Discord OAuth2 redirect target
#[utoipa::path(
    get,
    path = "/v1/auth/discord/callback",
    tag = "auth",
    operation_id = "discordCallback",
    security(()),
    params(CallbackParams),
    responses(
        (status = 302, description = "Redirect to the launcher deep link or the admin UI", headers(("Location" = String))),
        BadRequest,
        Forbidden
    )
)]
pub async fn callback(
    State(state): State<AppState>,
    meta: RequestMeta,
    params: Result<AxumQuery<CallbackParams>, QueryRejection>,
) -> ApiResult<Response> {
    // Discord may add parameters, so this is axum's lenient query parser; its rejection is
    // plain text, so a missing or malformed state becomes the usual problem.
    let AxumQuery(params) = params.map_err(|_| invalid_state())?;
    if params.state.len() > 128 {
        return Err(invalid_state());
    }
    let flow = sqlx::query!(
        r#"UPDATE oauth_flows SET consumed_at = now()
           WHERE state_hash = $1 AND consumed_at IS NULL AND expires_at > now()
           RETURNING client_kind, code_challenge, client_state, device_name, return_to"#,
        tokens::digest(&params.state)
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(invalid_state)?;

    if params.error.is_some() {
        return Err(ApiError::forbidden_code(
            "discord_denied",
            "Sign-in was cancelled in Discord",
        ));
    }
    let code = params
        .code
        .filter(|c| !c.is_empty() && c.len() <= 256)
        .ok_or_else(|| ApiError::bad_request("invalid_code", "The sign-in code is missing"))?;
    let profile = discord::fetch_profile(&state, &code).await?;
    let user = admit_user(&state, &profile).await?;

    if flow.client_kind == "web" {
        let session = tokens::new_token(WEB_SESSION_PREFIX)?;
        let csrf = tokens::random_b64(32)?;
        sqlx::query!(
            r#"INSERT INTO sessions (user_id, kind, access_token_hash, access_expires_at, csrf_token_hash, user_agent, ip)
               VALUES ($1, 'web', $2, $3, $4, $5, $6::text::inet)"#,
            user.id,
            tokens::digest(&session),
            OffsetDateTime::now_utc() + WEB_IDLE,
            tokens::digest(&csrf),
            meta.user_agent,
            meta.ip.map(|ip| ip.to_string())
        )
        .execute(&state.db)
        .await?;
        let max_age = WEB_ABSOLUTE.whole_seconds();
        let target = format!(
            "{}{}",
            state.config.public_origin(),
            flow.return_to.as_deref().unwrap_or("/admin/")
        );
        let mut resp = Redirect::to(&target).into_response();
        *resp.status_mut() = StatusCode::FOUND;
        append_cookie(
            resp.headers_mut(),
            &format!(
                "{SESSION_COOKIE}={session}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={max_age}"
            ),
        );
        append_cookie(
            resp.headers_mut(),
            &format!("{CSRF_COOKIE}={csrf}; Path=/; Secure; SameSite=Lax; Max-Age={max_age}"),
        );
        return Ok(resp);
    }

    let challenge = flow.code_challenge.ok_or_else(ApiError::internal)?;
    let login_code = tokens::random_b64(32)?;
    sqlx::query!(
        r#"INSERT INTO login_codes (code_hash, user_id, code_challenge, device_name, expires_at)
           VALUES ($1, $2, $3, $4, $5)"#,
        tokens::digest(&login_code),
        user.id,
        challenge,
        flow.device_name,
        OffsetDateTime::now_utc() + LOGIN_CODE_TTL
    )
    .execute(&state.db)
    .await?;
    let mut deep_link =
        url::Url::parse("vgames://auth/callback").map_err(ApiError::internal_from)?;
    {
        let mut q = deep_link.query_pairs_mut();
        q.append_pair("code", &login_code);
        if let Some(cs) = &flow.client_state {
            q.append_pair("client_state", cs);
        }
    }
    let body = fallback_page(deep_link.as_str(), &login_code);
    let mut resp = (StatusCode::FOUND, Html(body)).into_response();
    resp.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(deep_link.as_str()).map_err(ApiError::internal_from)?,
    );
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(resp)
}

fn invalid_state() -> ApiError {
    ApiError::bad_request(
        "invalid_state",
        "This sign-in link has expired or was already used; start again",
    )
}

fn append_cookie(headers: &mut HeaderMap, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        headers.append(header::SET_COOKIE, v);
    }
}

/// Shown when the browser cannot hand the deep link to the launcher.
fn fallback_page(deep_link: &str, code: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Signed in to vgames</title>
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>body{{font:16px/1.5 system-ui,sans-serif;max-width:36rem;margin:3rem auto;padding:0 1rem}}code{{display:block;padding:.75rem;background:#eef0f5;border-radius:6px;word-break:break-all}}</style>
</head><body><h1>You're signed in</h1>
<p>Your browser should now return you to vgames. If nothing happens, <a href="{deep_link}">open vgames</a>,
or paste this code into the launcher's "Paste code" field within one minute:</p>
<code>{code}</code></body></html>"#
    )
}

struct AdmittedUser {
    id: Uuid,
}

/// Applies the registration policy, then creates or refreshes the user.
async fn admit_user(
    state: &AppState,
    profile: &discord::DiscordProfile,
) -> ApiResult<AdmittedUser> {
    if !(5..=25).contains(&profile.id.len()) || !profile.id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::internal_from(
            "discord returned an invalid user id",
        ));
    }
    let is_bootstrap =
        state.config.bootstrap_owner_discord_id.as_deref() == Some(profile.id.as_str());
    let existing = sqlx::query!(
        "SELECT id, disabled_at FROM users WHERE discord_id = $1",
        profile.id
    )
    .fetch_optional(&state.db)
    .await?;
    match &existing {
        Some(u) if u.disabled_at.is_some() => {
            return Err(ApiError::forbidden_code(
                "user_disabled",
                "This account is disabled",
            ));
        }
        Some(_) => {}
        None if is_bootstrap => {}
        None => match crate::settings::load(&state.db).await?.registration_mode {
            RegistrationMode::Open => {}
            RegistrationMode::Allowlist => {
                let listed = sqlx::query_scalar!(
                    r#"SELECT EXISTS (SELECT 1 FROM registration_allowlist WHERE discord_id = $1) AS "e!""#,
                    profile.id
                )
                .fetch_one(&state.db)
                .await?;
                if !listed {
                    return Err(ApiError::forbidden_code(
                        "not_allowlisted",
                        "This server only admits invited Discord accounts",
                    ));
                }
            }
            RegistrationMode::Closed => {
                return Err(ApiError::forbidden_code(
                    "registration_closed",
                    "This server is not accepting new accounts",
                ));
            }
        },
    }

    let username: String = profile.username.chars().take(64).collect();
    let username = if username.is_empty() {
        "user".to_string()
    } else {
        username
    };
    let display_name = profile
        .global_name
        .as_ref()
        .map(|n| n.chars().take(64).collect::<String>())
        .filter(|n| !n.is_empty());
    let avatar = profile.avatar.clone().filter(|a| {
        let h = a.strip_prefix("a_").unwrap_or(a);
        h.len() == 32
            && h.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    });

    let mut tx = state.db.begin().await?;
    let row = sqlx::query!(
        r#"INSERT INTO users (discord_id, username, display_name, avatar_hash, role, last_seen_at)
           VALUES ($1, $2, $3, $4, CASE WHEN $5 THEN 'owner' ELSE 'user' END, now())
           ON CONFLICT (discord_id) DO UPDATE
             SET username = EXCLUDED.username, display_name = EXCLUDED.display_name,
                 avatar_hash = EXCLUDED.avatar_hash, last_seen_at = now()
           RETURNING id, role"#,
        profile.id,
        username,
        display_name,
        avatar,
        is_bootstrap
    )
    .fetch_one(&mut *tx)
    .await?;
    // The bootstrap account becomes owner if the server has none (e.g. it signed in before
    // VGAMES_BOOTSTRAP_OWNER_DISCORD_ID was set).
    if is_bootstrap && row.role != "owner" {
        let promoted = sqlx::query!(
            r#"UPDATE users SET role = 'owner' WHERE id = $1 AND NOT EXISTS (SELECT 1 FROM users WHERE role = 'owner')"#,
            row.id
        )
        .execute(&mut *tx)
        .await?;
        if promoted.rows_affected() == 1 {
            sqlx::query!(
                r#"INSERT INTO audit_log (actor_user_id, action, target_type, target_id, details)
                   VALUES ($1, 'user.bootstrap_owner', 'user', $2, '{}')"#,
                row.id,
                row.id.to_string()
            )
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(AdmittedUser { id: row.id })
}

/// Exchange a login code or refresh token for tokens
#[utoipa::path(
    post,
    path = "/v1/auth/token",
    tag = "auth",
    operation_id = "exchangeToken",
    security(()),
    request_body = TokenRequest,
    responses((status = 200, description = "New token pair", body = TokenResponse), BadRequest, Unauthorized, TooManyRequests)
)]
pub async fn token(
    State(state): State<AppState>,
    meta: RequestMeta,
    Json(req): Json<TokenRequest>,
) -> ApiResult<axum::Json<TokenResponse>> {
    let invalid_grant = || {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_grant",
            "The sign-in code or refresh token is invalid",
        )
    };
    let access = tokens::new_token(ACCESS_PREFIX)?;
    let refresh = tokens::new_token(REFRESH_PREFIX)?;
    let now = OffsetDateTime::now_utc();

    let user_id = match req {
        TokenRequest::AuthorizationCode {
            code,
            code_verifier,
        } => {
            let row = sqlx::query!(
                r#"UPDATE login_codes SET consumed_at = now()
                   WHERE code_hash = $1 AND consumed_at IS NULL AND expires_at > now()
                   RETURNING user_id, code_challenge, device_name"#,
                tokens::digest(&code)
            )
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(invalid_grant)?;
            if !tokens::pkce_matches(&code_verifier, &row.code_challenge) {
                return Err(invalid_grant());
            }
            ensure_enabled(&state, row.user_id).await?;
            sqlx::query!(
                r#"INSERT INTO sessions (user_id, kind, access_token_hash, access_expires_at, refresh_token_hash,
                                         refresh_expires_at, device_name, user_agent, ip)
                   VALUES ($1, 'desktop', $2, $3, $4, $5, $6, $7, $8::text::inet)"#,
                row.user_id,
                tokens::digest(&access),
                now + ACCESS_TTL,
                tokens::digest(&refresh),
                now + REFRESH_TTL,
                row.device_name,
                meta.user_agent,
                meta.ip.map(|ip| ip.to_string())
            )
            .execute(&state.db)
            .await?;
            row.user_id
        }
        TokenRequest::RefreshToken { refresh_token } => {
            let old = tokens::digest(&refresh_token);
            let rotated = sqlx::query!(
                r#"UPDATE sessions
                   SET access_token_hash = $2, access_expires_at = $3,
                       previous_refresh_token_hash = refresh_token_hash, refresh_token_hash = $4,
                       refresh_expires_at = LEAST($5, created_at + make_interval(secs => $6)),
                       last_used_at = now()
                   WHERE refresh_token_hash = $1 AND revoked_at IS NULL AND kind = 'desktop'
                     AND refresh_expires_at > now()
                   RETURNING user_id"#,
                old,
                tokens::digest(&access),
                now + ACCESS_TTL,
                tokens::digest(&refresh),
                now + REFRESH_TTL,
                REFRESH_ABSOLUTE.as_seconds_f64()
            )
            .fetch_optional(&state.db)
            .await?;
            match rotated {
                Some(r) => {
                    ensure_enabled(&state, r.user_id).await?;
                    r.user_id
                }
                None => return Err(refresh_failure(&state, &old).await?),
            }
        }
    };

    let user = load_user(&state, user_id).await?;
    Ok(axum::Json(TokenResponse {
        access_token: access,
        refresh_token: refresh,
        token_type: "Bearer".to_string(),
        expires_in: ACCESS_TTL.whole_seconds(),
        user,
    }))
}

/// Why a refresh failed: reuse of a rotated-out token revokes the whole session.
async fn refresh_failure(state: &AppState, old: &[u8]) -> ApiResult<ApiError> {
    let reused = sqlx::query!(
        r#"UPDATE sessions SET revoked_at = now(), revoked_reason = 'refresh_reuse'
           WHERE previous_refresh_token_hash = $1 AND revoked_at IS NULL
           RETURNING id, user_id"#,
        old
    )
    .fetch_optional(&state.db)
    .await?;
    if let Some(row) = reused {
        let session_id = row.id;
        extract::notify_revoked(
            crate::realtime::bus::revoke_sessions(
                state,
                row.user_id,
                &[session_id],
                "refresh_reuse",
            )
            .await,
        );
        tracing::warn!(%session_id, "refresh token reuse detected; session revoked");
        return Ok(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "refresh_token_reused",
            "This sign-in was used from another place and has been ended for safety; sign in again",
        ));
    }
    let expired = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM sessions WHERE refresh_token_hash = $1 AND revoked_at IS NULL) AS "e!""#,
        old
    )
    .fetch_one(&state.db)
    .await?;
    Ok(if expired {
        ApiError::session_expired()
    } else {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_grant",
            "The sign-in code or refresh token is invalid",
        )
    })
}

async fn ensure_enabled(state: &AppState, user_id: Uuid) -> ApiResult<()> {
    let disabled = sqlx::query_scalar!(
        r#"SELECT disabled_at IS NOT NULL AS "d!" FROM users WHERE id = $1"#,
        user_id
    )
    .fetch_one(&state.db)
    .await?;
    if disabled {
        extract::revoke_all_for_user(state, user_id, "user_disabled").await?;
        return Err(ApiError::forbidden_code(
            "user_disabled",
            "This account is disabled",
        ));
    }
    Ok(())
}

pub async fn load_user(state: &AppState, user_id: Uuid) -> ApiResult<User> {
    let r = sqlx::query!(
        "SELECT id, discord_id, username, display_name, avatar_hash, role, created_at FROM users WHERE id = $1",
        user_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok(User {
        avatar_url: discord::avatar_url(&r.discord_id, r.avatar_hash.as_deref()),
        id: r.id,
        discord_id: Some(r.discord_id),
        username: r.username,
        display_name: r.display_name,
        role: parse_role(&r.role),
        created_at: r.created_at,
    })
}

/// Revoke the current session
#[utoipa::path(
    post,
    path = "/v1/auth/logout",
    tag = "auth",
    operation_id = "logout",
    responses((status = 204, description = "Signed out"), Unauthorized)
)]
pub async fn logout(State(state): State<AppState>, user: CurrentUser) -> ApiResult<Response> {
    sqlx::query!(
        "UPDATE sessions SET revoked_at = now(), revoked_reason = 'logout' WHERE id = $1 AND revoked_at IS NULL",
        user.session_id
    )
    .execute(&state.db)
    .await?;
    extract::notify_revoked(
        crate::realtime::bus::revoke_sessions(&state, user.user_id, &[user.session_id], "logout")
            .await,
    );
    let mut resp = StatusCode::NO_CONTENT.into_response();
    if user.kind == ClientKind::Web {
        append_cookie(
            resp.headers_mut(),
            &format!("{SESSION_COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0"),
        );
        append_cookie(
            resp.headers_mut(),
            &format!("{CSRF_COOKIE}=; Path=/; Secure; SameSite=Lax; Max-Age=0"),
        );
    }
    Ok(resp)
}

async fn session_dto(state: &AppState, session_id: Uuid, current: Uuid) -> ApiResult<Session> {
    let r = sqlx::query!(
        "SELECT id, kind, device_name, user_agent, created_at, last_used_at FROM sessions WHERE id = $1",
        session_id
    )
    .fetch_one(&state.db)
    .await?;
    Ok(Session {
        id: r.id,
        kind: if r.kind == "web" {
            ClientKind::Web
        } else {
            ClientKind::Desktop
        },
        device_name: r.device_name,
        user_agent: r.user_agent,
        created_at: r.created_at,
        last_used_at: r.last_used_at,
        current: r.id == current,
    })
}

/// Current user and session
#[utoipa::path(
    get,
    path = "/v1/me",
    tag = "auth",
    operation_id = "getMe",
    responses((status = 200, description = "Current user", body = Me), Unauthorized)
)]
pub async fn me(State(state): State<AppState>, user: CurrentUser) -> ApiResult<axum::Json<Me>> {
    Ok(axum::Json(Me {
        user: load_user(&state, user.user_id).await?,
        session: session_dto(&state, user.session_id, user.session_id).await?,
        device_id: user.device_id,
        csrf_token: None,
    }))
}

/// List own active sessions
#[utoipa::path(
    get,
    path = "/v1/me/sessions",
    tag = "auth",
    operation_id = "listMySessions",
    responses((status = 200, description = "Sessions", body = SessionList), Unauthorized)
)]
pub async fn list_sessions(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<axum::Json<SessionList>> {
    let rows = sqlx::query!(
        r#"SELECT id, kind, device_name, user_agent, created_at, last_used_at FROM sessions
           WHERE user_id = $1 AND revoked_at IS NULL
             AND ((kind = 'web' AND access_expires_at > now()) OR (kind = 'desktop' AND refresh_expires_at > now()))
           ORDER BY last_used_at DESC
           LIMIT 200"#,
        user.user_id
    )
    .fetch_all(&state.db)
    .await?;
    Ok(axum::Json(SessionList {
        items: rows
            .into_iter()
            .map(|r| Session {
                current: r.id == user.session_id,
                id: r.id,
                kind: if r.kind == "web" {
                    ClientKind::Web
                } else {
                    ClientKind::Desktop
                },
                device_name: r.device_name,
                user_agent: r.user_agent,
                created_at: r.created_at,
                last_used_at: r.last_used_at,
            })
            .collect(),
    }))
}

/// Revoke one of own sessions
#[utoipa::path(
    delete,
    path = "/v1/me/sessions/{session_id}",
    tag = "auth",
    operation_id = "revokeMySession",
    params(("session_id" = Uuid, Path)),
    responses((status = 204, description = "Revoked"), Unauthorized, NotFound)
)]
pub async fn revoke_session(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(session_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let done = sqlx::query!(
        r#"UPDATE sessions SET revoked_at = now(), revoked_reason = 'logout'
           WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL"#,
        session_id,
        user.user_id
    )
    .execute(&state.db)
    .await?;
    if done.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    extract::notify_revoked(
        crate::realtime::bus::revoke_sessions(&state, user.user_id, &[session_id], "logout").await,
    );
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------
// Dev-only fake Discord
// ---------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct FakeParams {
    state: String,
}

async fn fake_page(
    State(state): State<AppState>,
    AxumQuery(p): AxumQuery<FakeParams>,
) -> Html<String> {
    let default_id = state
        .config
        .bootstrap_owner_discord_id
        .clone()
        .unwrap_or_else(|| "100000000000000002".to_string());
    let st: String = p
        .state
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(128)
        .collect();
    Html(format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Fake Discord (dev)</title></head>
<body style="font:16px system-ui;max-width:30rem;margin:3rem auto">
<h1>Fake Discord sign-in</h1><p>Development only. Pick the Discord account to sign in as.</p>
<form action="/v1/auth/dev/fake-discord/submit" method="get">
<input type="hidden" name="state" value="{st}">
<p><label>Discord id <input name="id" value="{default_id}" pattern="[0-9]{{5,25}}" required></label></p>
<p><label>Username <input name="name" value="devuser" pattern="[A-Za-z0-9_]{{1,32}}" required></label></p>
<p><button>Sign in</button></p></form></body></html>"#
    ))
}

#[derive(Deserialize)]
struct FakeSubmit {
    state: String,
    id: String,
    name: String,
}

async fn fake_submit(
    State(state): State<AppState>,
    AxumQuery(p): AxumQuery<FakeSubmit>,
) -> ApiResult<Redirect> {
    let mut url = url::Url::parse(&format!(
        "{}/v1/auth/discord/callback",
        state.config.public_origin()
    ))
    .map_err(ApiError::internal_from)?;
    url.query_pairs_mut()
        .append_pair("state", &p.state)
        .append_pair("code", &format!("fake.{}.{}", p.id, p.name));
    Ok(Redirect::to(url.as_str()))
}
