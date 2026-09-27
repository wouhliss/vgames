//! Realtime WebSocket protocol (docs/architecture/03-api.md §6).
//!
//! Owner: Agent 4 (typed social events below). Agent 1 created the
//! envelope and ticket types the gateway needs (A1-T05).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Every frame, in both directions: `{ "v": 1, "id", "type", "ts", "data" }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u8,
    /// UUIDv7; absent on client frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub ts: Option<OffsetDateTime>,
    #[serde(default)]
    pub data: serde_json::Value,
}

/// `POST /v1/realtime/ticket` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RealtimeTicket {
    pub ticket: String,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub expires_at: OffsetDateTime,
}

/// `hello` event data, sent first on every connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub user_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub server_time: OffsetDateTime,
}

/// `session.revoked` event data; the server closes the socket right after.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRevoked {
    pub reason: String,
}

/// Close codes the gateway uses (besides standard 1000/1001/1009/1012).
pub mod close {
    /// The session was revoked (sign-out elsewhere, admin action, token reuse).
    pub const SESSION_REVOKED: u16 = 4001;
    /// The client could not keep up; reconnect and resync over REST.
    pub const TOO_SLOW: u16 = 4002;
    /// A frame that is not a valid envelope.
    pub const PROTOCOL: u16 = 4003;
}

// ---------------------------------------------------------------------------------------
// Social events (Agent 4, 05-social-notes §4)
// ---------------------------------------------------------------------------------------

/// Realtime event type names used by the social features.
pub mod kinds {
    pub const PRESENCE_CHANGED: &str = "presence.changed";
    pub const FRIEND_REQUEST: &str = "friend.request";
    pub const FRIEND_ACCEPTED: &str = "friend.accepted";
    pub const FRIEND_REMOVED: &str = "friend.removed";
    pub const INBOX_NEW: &str = "inbox.new";
    pub const INVITE_CREATED: &str = "invite.created";
    pub const INVITE_UPDATED: &str = "invite.updated";
    pub const DEVICE_ADDED: &str = "device.added";
    pub const DEVICE_REVOKED: &str = "device.revoked";
    pub const TYPING: &str = "typing";
    /// Sent right before the server closes a revoked session's socket with 4001.
    pub const SESSION_REVOKED: &str = "session.revoked";
    /// Client → server.
    pub const PRESENCE_SET: &str = "presence.set";
}

/// `presence.changed` data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceChanged {
    pub user_id: Uuid,
    pub status: crate::social::PresenceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_title: Option<String>,
}

/// `friend.request` / `friend.accepted` / `friend.removed` data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FriendEvent {
    pub user_id: Uuid,
}

/// `inbox.new` data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxNew {
    pub conversation_id: Uuid,
    pub count: i64,
}

/// `invite.created` / `invite.updated` data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InviteEvent {
    pub invite: crate::social::Invite,
}

/// `device.added` / `device.revoked` data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceEvent {
    pub user_id: Uuid,
    pub device_id: Uuid,
}

/// Client → server `typing` data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypingStart {
    pub conversation_id: Uuid,
}

/// Server → client `typing` data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Typing {
    pub conversation_id: Uuid,
    pub user_id: Uuid,
}
