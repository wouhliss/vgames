//! Realtime WebSocket protocol (docs/architecture/03-api.md §6).
//!
//! Owner: Agent 4 (extends this module with typed social events). Agent 1 created the
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
