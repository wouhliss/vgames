//! Social commands and events for the main window (05-social-notes §5–§6, A4-T07).
//! Thin wrappers: the logic lives in [`SocialService`].

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager, State};
use tauri_specta::Event as _;
use uuid::Uuid;

use super::model::{
    BlockedUser, ContactSecurity, Conversation, DeviceNotice, Friend, FriendCode, FriendList,
    FriendTarget, Message, MessageStatus, MyDevice, Presence, SocialConnection, SocialError,
    SocialSettings, UserSummary,
};
use super::ports::SystemIdle;
use super::realtime::Timing;
use super::service::{SocialEvents, SocialService, default_device_name};
use super::{secrets, store::Keys};
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

/// `conversations-changed`: membership, ordering or unread counts changed.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct ConversationsChanged(pub Vec<Conversation>);

/// `message-received`: a decrypted incoming message (or a device notice) was stored.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct MessageReceived(pub Message);

/// `message-status-changed`: an outgoing message was sent or failed.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct MessageStatusChanged {
    pub message_id: Uuid,
    pub conversation_id: Uuid,
    pub status: MessageStatus,
}

/// `typing`: show "… is typing" for 5 s.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct Typing {
    pub conversation_id: Uuid,
    pub user_id: Uuid,
}

/// `device-notice`: a contact's (or your own) new device, key change or revocation.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
#[tauri_specta(event_name = "device-notice")]
pub struct DeviceNoticeEvent {
    pub conversation_id: Option<Uuid>,
    pub notice: DeviceNotice,
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
    fn conversations_changed(&self, conversations: &[Conversation]) {
        let _ = ConversationsChanged(conversations.to_vec()).emit(&self.0);
    }
    fn message_received(&self, message: &Message) {
        let _ = MessageReceived(message.clone()).emit(&self.0);
    }
    fn message_status_changed(
        &self,
        message_id: Uuid,
        conversation_id: Uuid,
        status: MessageStatus,
    ) {
        let _ = MessageStatusChanged {
            message_id,
            conversation_id,
            status,
        }
        .emit(&self.0);
    }
    fn typing(&self, conversation_id: Uuid, user_id: Uuid) {
        let _ = Typing {
            conversation_id,
            user_id,
        }
        .emit(&self.0);
    }
    fn device_notice(&self, conversation_id: Option<Uuid>, notice: &DeviceNotice) {
        let _ = DeviceNoticeEvent {
            conversation_id,
            notice: notice.clone(),
        }
        .emit(&self.0);
    }
}

/// Starts the social core and makes it available to commands (called from `setup`).
pub fn init(app: &AppHandle, state: &AppState) -> Result<(), SocialError> {
    // Keychain entries are per app identifier, so debug profiles keep separate keys.
    let secrets = secrets::open(&app.config().identifier, &state.paths.data_dir);
    let keys = Keys::from_secret_store(secrets.as_ref())
        .map_err(|e| SocialError::internal("loading the chat keys", &e))?;
    let service = tauri::async_runtime::block_on(SocialService::start(
        super::ports::SessionSlot::new(),
        state.db.clone(),
        &state.bus,
        Arc::new(TauriEvents(app.clone())),
        Arc::new(SystemIdle),
        Arc::new(keys),
        default_device_name(),
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

// ---- messaging (A4-T08) ---------------------------------------------------------------------

/// Conversations, most recent first (the cached list when offline).
#[tauri::command]
#[specta::specta]
pub async fn conversations_list(
    social: State<'_, SocialService>,
) -> Result<Vec<Conversation>, SocialError> {
    social.conversations_list().await
}

/// The direct conversation with a friend (created on first use).
#[tauri::command]
#[specta::specta]
pub async fn conversation_open_direct(
    social: State<'_, SocialService>,
    user_id: Uuid,
) -> Result<Conversation, SocialError> {
    social.conversation_open_direct(user_id).await
}

/// A party with 1–15 friends.
#[tauri::command]
#[specta::specta]
pub async fn conversation_create_party(
    social: State<'_, SocialService>,
    user_ids: Vec<Uuid>,
) -> Result<Conversation, SocialError> {
    social.conversation_create_party(user_ids).await
}

/// Messages oldest first; `limit` 1–200, before the message `before` if given.
#[tauri::command]
#[specta::specta]
pub async fn messages_list(
    social: State<'_, SocialService>,
    conversation_id: Uuid,
    before: Option<Uuid>,
    limit: u32,
) -> Result<Vec<Message>, SocialError> {
    social.messages_list(conversation_id, before, limit).await
}

/// Sends a text message (`pending`, then `message-status-changed`).
#[tauri::command]
#[specta::specta]
pub async fn message_send(
    social: State<'_, SocialService>,
    conversation_id: Uuid,
    text: String,
) -> Result<Message, SocialError> {
    social.message_send(conversation_id, text).await
}

#[tauri::command]
#[specta::specta]
pub async fn message_retry(
    social: State<'_, SocialService>,
    message_id: Uuid,
) -> Result<Message, SocialError> {
    social.message_retry(message_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn conversation_mark_read(
    social: State<'_, SocialService>,
    conversation_id: Uuid,
) -> Result<(), SocialError> {
    social.conversation_mark_read(conversation_id).await
}

/// "I am typing" (throttled to one frame every 3 s per conversation).
#[tauri::command]
#[specta::specta]
pub fn typing_start(
    social: State<'_, SocialService>,
    conversation_id: Uuid,
) -> Result<(), SocialError> {
    social.typing_start(conversation_id)
}

/// Safety number, verification state and devices of a contact.
#[tauri::command]
#[specta::specta]
pub async fn contact_security(
    social: State<'_, SocialService>,
    user_id: Uuid,
) -> Result<ContactSecurity, SocialError> {
    social.contact_security(user_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn contact_set_verified(
    social: State<'_, SocialService>,
    user_id: Uuid,
    verified: bool,
) -> Result<ContactSecurity, SocialError> {
    social.contact_set_verified(user_id, verified).await
}

/// Accepts a contact device's changed key and unblocks sending to it.
#[tauri::command]
#[specta::specta]
pub async fn contact_trust_device(
    social: State<'_, SocialService>,
    user_id: Uuid,
    device_id: Uuid,
) -> Result<ContactSecurity, SocialError> {
    social.contact_trust_device(user_id, device_id).await
}

/// This account's devices on the active server.
#[tauri::command]
#[specta::specta]
pub async fn devices_list(social: State<'_, SocialService>) -> Result<Vec<MyDevice>, SocialError> {
    social.devices_list().await
}

#[tauri::command]
#[specta::specta]
pub async fn device_revoke(
    social: State<'_, SocialService>,
    device_id: Uuid,
) -> Result<(), SocialError> {
    social.device_revoke(device_id).await
}
