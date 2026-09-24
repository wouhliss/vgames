//! Discord OAuth2 (confidential client, scope `identify`) and the dev-only fake provider.

use serde::Deserialize;
use url::Url;

use crate::{
    config::{DiscordApp, DiscordConfig},
    error::ApiError,
    state::AppState,
};

/// The Discord profile fields vgames keeps.
#[derive(Debug, Clone)]
pub struct DiscordProfile {
    pub id: String,
    pub username: String,
    pub global_name: Option<String>,
    pub avatar: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct MeResponse {
    id: String,
    username: String,
    global_name: Option<String>,
    avatar: Option<String>,
}

/// URL the browser opens to consent.
pub fn authorize_url(state: &AppState, oauth_state: &str) -> Result<String, ApiError> {
    match &state.config.discord {
        DiscordConfig::Discord(app) => {
            let mut url = app
                .api_base
                .join("/oauth2/authorize")
                .map_err(ApiError::internal_from)?;
            url.query_pairs_mut()
                .append_pair("response_type", "code")
                .append_pair("client_id", &app.client_id)
                .append_pair("scope", "identify")
                .append_pair("state", oauth_state)
                .append_pair("redirect_uri", app.redirect_uri.as_str())
                .append_pair("prompt", "none");
            Ok(url.into())
        }
        DiscordConfig::Fake => {
            let mut url = Url::parse(&format!(
                "{}/v1/auth/dev/fake-discord",
                state.config.public_origin()
            ))
            .map_err(ApiError::internal_from)?;
            url.query_pairs_mut().append_pair("state", oauth_state);
            Ok(url.into())
        }
    }
}

/// Exchanges the authorization code and fetches the profile. The Discord access token is
/// dropped as soon as the profile is read; it is never stored or logged.
pub async fn fetch_profile(state: &AppState, code: &str) -> Result<DiscordProfile, ApiError> {
    match &state.config.discord {
        DiscordConfig::Discord(app) => fetch_real(state, app, code).await,
        DiscordConfig::Fake => parse_fake_code(code),
    }
}

async fn fetch_real(
    state: &AppState,
    app: &DiscordApp,
    code: &str,
) -> Result<DiscordProfile, ApiError> {
    let unavailable = |e: &dyn std::fmt::Display| {
        tracing::warn!(error = %e, "discord request failed");
        ApiError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "discord_unavailable",
            "Discord could not be reached; try again",
        )
    };
    let token_url = app
        .api_base
        .join("/api/oauth2/token")
        .map_err(ApiError::internal_from)?;
    let resp = state
        .http
        .post(token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", app.redirect_uri.as_str()),
            ("client_id", app.client_id.as_str()),
            ("client_secret", app.client_secret.expose().as_str()),
        ])
        .send()
        .await
        .map_err(|e| unavailable(&e))?;
    if resp.status().is_client_error() {
        return Err(ApiError::forbidden_code(
            "discord_code_invalid",
            "Discord rejected the sign-in; start again",
        ));
    }
    if !resp.status().is_success() {
        return Err(unavailable(&resp.status()));
    }
    let token: TokenResponse = resp.json().await.map_err(|e| unavailable(&e))?;
    let me_url = app
        .api_base
        .join("/api/users/@me")
        .map_err(ApiError::internal_from)?;
    let resp = state
        .http
        .get(me_url)
        .bearer_auth(&token.access_token)
        .send()
        .await
        .map_err(|e| unavailable(&e))?;
    drop(token);
    if !resp.status().is_success() {
        return Err(unavailable(&resp.status()));
    }
    let me: MeResponse = resp.json().await.map_err(|e| unavailable(&e))?;
    Ok(DiscordProfile {
        id: me.id,
        username: me.username,
        global_name: me.global_name,
        avatar: me.avatar,
    })
}

/// Fake codes are `fake.<discord id>.<username>` (debug builds only).
fn parse_fake_code(code: &str) -> Result<DiscordProfile, ApiError> {
    let invalid = || ApiError::bad_request("invalid_code", "The sign-in code is invalid");
    if !cfg!(debug_assertions) {
        return Err(invalid());
    }
    let rest = code.strip_prefix("fake.").ok_or_else(invalid)?;
    let (id, name) = rest.split_once('.').ok_or_else(invalid)?;
    if !(5..=25).contains(&id.len()) || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let username: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(32)
        .collect();
    if username.is_empty() {
        return Err(invalid());
    }
    Ok(DiscordProfile {
        id: id.to_string(),
        username,
        global_name: None,
        avatar: None,
    })
}

/// Avatar URL on Discord's CDN, when the user has one.
pub fn avatar_url(discord_id: &str, avatar_hash: Option<&str>) -> Option<String> {
    avatar_hash.map(|h| {
        let ext = if h.starts_with("a_") { "gif" } else { "png" };
        format!("https://cdn.discordapp.com/avatars/{discord_id}/{h}.{ext}")
    })
}
