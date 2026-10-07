//! Account commands (INS-05): where this account is signed in on a server, and signing one of those
//! sessions out. Signing out this launcher's own session forgets it locally too.

use reqwest::Method;
use serde::Serialize;
use specta::Type;
use tauri::State;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;
use vgames_proto::auth::{ClientKind, Session, SessionList};

use crate::error::AppError;
use crate::servers::Servers;
use crate::state::AppState;

/// Which kind of client holds a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SessionClient {
    Desktop,
    Web,
}

/// A signed-in session of this account on one server (`GET /v1/me/sessions`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct AccountSession {
    pub id: String,
    pub client: SessionClient,
    /// The name the device gave when it signed in, if any (plain text from the server).
    pub device_name: Option<String>,
    /// The browser or launcher that signed in, if the server recorded it (plain text).
    pub user_agent: Option<String>,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339.
    pub last_used_at: String,
    /// This launcher's own session.
    pub current: bool,
}

fn rfc3339(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap_or_default()
}

impl From<Session> for AccountSession {
    fn from(session: Session) -> Self {
        Self {
            id: session.id.to_string(),
            client: match session.kind {
                ClientKind::Desktop => SessionClient::Desktop,
                ClientKind::Web => SessionClient::Web,
            },
            device_name: session.device_name,
            user_agent: session.user_agent,
            created_at: rfc3339(session.created_at),
            last_used_at: rfc3339(session.last_used_at),
            current: session.current,
        }
    }
}

pub(crate) async fn sessions(
    servers: &Servers,
    server_id: Uuid,
) -> Result<Vec<AccountSession>, AppError> {
    let api = servers.api(server_id).await?;
    let list: SessionList = api
        .authed(Method::GET, "v1/me/sessions", None::<&()>)
        .await?;
    Ok(list.items.into_iter().map(AccountSession::from).collect())
}

pub(crate) async fn revoke(
    servers: &Servers,
    server_id: Uuid,
    session_id: Uuid,
) -> Result<(), AppError> {
    let api = servers.api(server_id).await?;
    // The list says which session is this launcher's; the server decides whether the id exists.
    let list: SessionList = api
        .authed(Method::GET, "v1/me/sessions", None::<&()>)
        .await?;
    let current = list.items.iter().any(|s| s.id == session_id && s.current);
    api.authed_empty(
        Method::DELETE,
        &format!("v1/me/sessions/{session_id}"),
        None::<&()>,
    )
    .await?;
    if current {
        // Our tokens are dead now: forget them without calling logout.
        servers.forget_account(server_id).await?;
    }
    Ok(())
}

/// Sessions of the signed-in account on `server_id`, this launcher's own marked `current`.
#[tauri::command]
#[specta::specta]
pub async fn account_sessions(
    state: State<'_, AppState>,
    server_id: Uuid,
) -> Result<Vec<AccountSession>, AppError> {
    sessions(&state.servers, server_id).await
}

/// Signs one session out on the server. Signing out this launcher's own session also forgets it
/// locally, as signing out does.
#[tauri::command]
#[specta::specta]
pub async fn account_session_revoke(
    state: State<'_, AppState>,
    server_id: Uuid,
    session_id: Uuid,
) -> Result<(), AppError> {
    revoke(&state.servers, server_id, session_id).await
}
