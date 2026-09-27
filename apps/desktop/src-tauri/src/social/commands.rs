//! Social commands and events for the main window (05-social-notes §5–§6, A4-T07).
//! Thin wrappers: the logic lives in [`SocialService`].

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager, State};
use tauri_specta::Event as _;
use uuid::Uuid;

use super::model::{
    BlockedUser, Friend, FriendCode, FriendList, FriendTarget, Presence, SocialConnection,
    SocialError, SocialSettings, UserSummary,
};
use super::ports::SystemIdle;
use super::realtime::Timing;
use super::service::{SocialEvents, SocialService};
use crate::state::AppState;

// ---- events ---------------------------------------------------------------------------------

/// `social-connection-changed`: the socket state changed.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct SocialConnectionChanged(pub SocialConnection);

/// `friends-changed`: the friend list after a resync.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct FriendsChanged(pub FriendList);

/// `presence-changed`: a friend's presence changed.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct PresenceChanged {
    pub user_id: Uuid,
    pub presence: Presence,
}

/// `friend-request-received`: someone sent you a request (toast).
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct FriendRequestReceived {
    pub user: UserSummary,
}

/// Emits social events to the WebView.
struct TauriEvents(AppHandle);

impl SocialEvents for TauriEvents {
    fn connection_changed(&self, connection: &SocialConnection) {
        let _ = SocialConnectionChanged(connection.clone()).emit(&self.0);
    }
    fn friends_changed(&self, friends: &FriendList) {
        let _ = FriendsChanged(friends.clone()).emit(&self.0);
    }
    fn presence_changed(&self, user_id: Uuid, presence: &Presence) {
        let _ = PresenceChanged {
            user_id,
            presence: presence.clone(),
        }
        .emit(&self.0);
    }
    fn friend_request_received(&self, user: &UserSummary) {
        let _ = FriendRequestReceived { user: user.clone() }.emit(&self.0);
    }
}

/// Starts the social core and makes it available to commands (called from `setup`).
pub fn init(app: &AppHandle, state: &AppState) -> Result<(), SocialError> {
    let service = tauri::async_runtime::block_on(SocialService::start(
        super::ports::SessionSlot::new(),
        state.db.clone(),
        &state.bus,
        Arc::new(TauriEvents(app.clone())),
        Arc::new(SystemIdle),
        Timing::default(),
        state.shutdown.child_token(),
    ))?;
    app.manage(service);
    Ok(())
}

// ---- commands -------------------------------------------------------------------------------

/// Socket state for the active server.
#[tauri::command]
#[specta::specta]
pub fn social_connection(social: State<'_, SocialService>) -> SocialConnection {
    social.connection()
}

#[tauri::command]
#[specta::specta]
pub async fn social_settings_get(
    social: State<'_, SocialService>,
) -> Result<SocialSettings, SocialError> {
    social.settings_get().await
}

#[tauri::command]
#[specta::specta]
pub async fn social_settings_set(
    social: State<'_, SocialService>,
    settings: SocialSettings,
) -> Result<SocialSettings, SocialError> {
    social.settings_set(settings).await
}

#[tauri::command]
#[specta::specta]
pub async fn friends_list(social: State<'_, SocialService>) -> Result<FriendList, SocialError> {
    social.friends_list().await
}

#[tauri::command]
#[specta::specta]
pub async fn friend_code_create(
    social: State<'_, SocialService>,
) -> Result<FriendCode, SocialError> {
    social.friend_code_create().await
}

/// Sends a request by friend code or user id (`accepted` if they had already asked you).
#[tauri::command]
#[specta::specta]
pub async fn friend_request_send(
    social: State<'_, SocialService>,
    target: FriendTarget,
) -> Result<Friend, SocialError> {
    social.friend_request_send(target).await
}

#[tauri::command]
#[specta::specta]
pub async fn friend_accept(
    social: State<'_, SocialService>,
    user_id: Uuid,
) -> Result<Friend, SocialError> {
    social.friend_accept(user_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn friend_decline(
    social: State<'_, SocialService>,
    user_id: Uuid,
) -> Result<(), SocialError> {
    social.friend_decline(user_id).await
}

/// Removes a friend or cancels an outgoing request.
#[tauri::command]
#[specta::specta]
pub async fn friend_remove(
    social: State<'_, SocialService>,
    user_id: Uuid,
) -> Result<(), SocialError> {
    social.friend_remove(user_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn user_block(
    social: State<'_, SocialService>,
    user_id: Uuid,
) -> Result<(), SocialError> {
    social.user_block(user_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn user_unblock(
    social: State<'_, SocialService>,
    user_id: Uuid,
) -> Result<(), SocialError> {
    social.user_unblock(user_id).await
}

/// Users blocked from this install on the active server (kept locally).
#[tauri::command]
#[specta::specta]
pub async fn blocks_list(
    social: State<'_, SocialService>,
) -> Result<Vec<BlockedUser>, SocialError> {
    social.blocks_list().await
}

#[tauri::command]
#[specta::specta]
pub async fn user_profile(
    social: State<'_, SocialService>,
    user_id: Uuid,
) -> Result<UserSummary, SocialError> {
    social.user_profile(user_id).await
}
