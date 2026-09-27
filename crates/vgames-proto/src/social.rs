//! Friends, presence, E2EE devices and messaging relay, and game invites
//! (docs/architecture/05-social.md, 05-social-notes.md). Owner: Agent 4.
//!
//! The server only ever sees ciphertext envelopes and metadata; every type here is
//! either metadata or opaque base64 ciphertext.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{auth::UserPublic, packages::PackageSummary};

// ---------------------------------------------------------------------------------------
// Presence
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PresenceStatus {
    Online,
    Away,
    InGame,
    Offline,
}

impl PresenceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PresenceStatus::Online => "online",
            PresenceStatus::Away => "away",
            PresenceStatus::InGame => "in_game",
            PresenceStatus::Offline => "offline",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "online" => PresenceStatus::Online,
            "away" => PresenceStatus::Away,
            "in_game" => PresenceStatus::InGame,
            "offline" => PresenceStatus::Offline,
            _ => return None,
        })
    }
}

/// A friend's presence as shown in the friend list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Presence {
    pub status: PresenceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_title: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = DateTime))]
    pub updated_at: Option<OffsetDateTime>,
}

/// States a client may set (offline comes from the socket going away).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PresenceSetStatus {
    Online,
    Away,
    InGame,
}

impl From<PresenceSetStatus> for PresenceStatus {
    fn from(s: PresenceSetStatus) -> Self {
        match s {
            PresenceSetStatus::Online => PresenceStatus::Online,
            PresenceSetStatus::Away => PresenceStatus::Away,
            PresenceSetStatus::InGame => PresenceStatus::InGame,
        }
    }
}

/// `PUT /v1/presence` body and realtime `presence.set` data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct PresenceUpdate {
    pub status: PresenceSetStatus,
    /// Only with `in_game`, and only when "show what I'm playing" is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_id: Option<Uuid>,
}

// ---------------------------------------------------------------------------------------
// Friends
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum FriendState {
    Accepted,
    Incoming,
    Outgoing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Friend {
    pub user: UserPublic,
    pub state: FriendState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<Presence>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = DateTime))]
    pub since: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FriendList {
    pub friends: Vec<Friend>,
    pub incoming: Vec<Friend>,
    pub outgoing: Vec<Friend>,
}

/// `POST /v1/friends/requests` body: exactly one of `user_id` or `friend_code`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(untagged)]
pub enum FriendRequestCreate {
    User(FriendRequestByUser),
    Code(FriendRequestByCode),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct FriendRequestByUser {
    pub user_id: Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct FriendRequestByCode {
    pub friend_code: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FriendCode {
    pub code: String,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub expires_at: OffsetDateTime,
}

/// Crockford base32 alphabet used by friend codes.
pub const FRIEND_CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const FRIEND_CODE_LEN: usize = 8;

/// `true` for a canonical friend code (`^[0-9A-HJKMNP-TV-Z]{8}$`).
pub fn is_friend_code(s: &str) -> bool {
    s.len() == FRIEND_CODE_LEN && s.bytes().all(|b| FRIEND_CODE_ALPHABET.contains(&b))
}

/// Normalizes what a person typed into a canonical friend code (Crockford decoding rules:
/// case-insensitive, `O`→`0`, `I`/`L`→`1`, spaces and hyphens ignored). `None` if invalid.
pub fn normalize_friend_code(input: &str) -> Option<String> {
    let mut out = String::with_capacity(FRIEND_CODE_LEN);
    for c in input.chars() {
        let c = match c.to_ascii_uppercase() {
            ' ' | '-' => continue,
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        };
        if !c.is_ascii() || !FRIEND_CODE_ALPHABET.contains(&(c as u8)) {
            return None;
        }
        out.push(c);
        if out.len() > FRIEND_CODE_LEN {
            return None;
        }
    }
    is_friend_code(&out).then_some(out)
}

// ---------------------------------------------------------------------------------------
// E2EE devices and key directory
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum OsFamily {
    Windows,
    Linux,
    Macos,
}

impl OsFamily {
    pub fn as_str(self) -> &'static str {
        match self {
            OsFamily::Windows => "windows",
            OsFamily::Linux => "linux",
            OsFamily::Macos => "macos",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "windows" => OsFamily::Windows,
            "linux" => OsFamily::Linux,
            "macos" => OsFamily::Macos,
            _ => return None,
        })
    }
}

/// One of the caller's own devices (`GET /v1/devices`, `POST /v1/devices`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Device {
    pub id: Uuid,
    pub display_name: String,
    pub platform: OsFamily,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub one_time_keys_available: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_fallback_key: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<bool>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = DateTime))]
    pub last_seen_at: Option<OffsetDateTime>,
}

/// `GET /v1/devices` response.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DeviceList {
    pub items: Vec<Device>,
}

/// `POST /v1/devices` body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct DeviceRegister {
    pub display_name: String,
    pub platform: OsFamily,
    /// Curve25519, unpadded base64 (43 chars).
    pub identity_key: String,
    /// Ed25519, unpadded base64 (43 chars).
    pub signing_key: String,
    /// Ed25519 signature by `signing_key` over [`canonical::device_keys`], unpadded base64 (86 chars).
    pub keys_signature: String,
}

/// A contact's device public keys (`GET /v1/users/{id}/devices`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DeviceKeys {
    pub device_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub identity_key: String,
    pub signing_key: String,
    pub keys_signature: String,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

/// `GET /v1/users/{id}/devices` response.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DeviceKeysList {
    pub items: Vec<DeviceKeys>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SignedOneTimeKey {
    pub key_id: String,
    pub public_key: String,
    /// Ed25519 signature over [`canonical::one_time_key`], unpadded base64 (86 chars).
    pub signature: String,
}

/// `POST /v1/devices/{id}/one-time-keys` body.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct OneTimeKeysUpload {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub one_time_keys: Vec<SignedOneTimeKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_key: Option<SignedOneTimeKey>,
}

/// `POST /v1/devices/{id}/one-time-keys` response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OneTimeKeysStored {
    pub available: i64,
}

/// `POST /v1/keys/claim` body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ClaimKeysRequest {
    pub device_ids: Vec<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ClaimedKey {
    pub device_id: Uuid,
    pub key_id: String,
    pub public_key: String,
    pub signature: String,
    pub is_fallback: bool,
}

/// `POST /v1/keys/claim` response: one key per claimable device (others are left out).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ClaimedKeyList {
    pub items: Vec<ClaimedKey>,
}

/// Most one-time keys a device may have unclaimed on the server (and upload at once).
pub const MAX_UNCLAIMED_ONE_TIME_KEYS: usize = 100;
/// Most devices per `POST /v1/keys/claim`.
pub const MAX_CLAIM_DEVICES: usize = 64;

// ---------------------------------------------------------------------------------------
// Conversations and the ciphertext relay
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    Direct,
    Party,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Conversation {
    pub id: Uuid,
    pub kind: ConversationKind,
    pub members: Vec<UserPublic>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = DateTime))]
    pub last_activity_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConversationPage {
    pub items: Vec<Conversation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// `POST /v1/conversations` body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConversationCreate {
    Direct { user_id: Uuid },
    Party { user_ids: Vec<Uuid> },
}

/// `type: integer, enum: [0, 1]` (0 = pre-key, 1 = normal Olm message).
#[cfg(feature = "openapi")]
fn olm_message_type() -> utoipa::openapi::schema::Object {
    use utoipa::openapi::schema::{ObjectBuilder, Type};
    ObjectBuilder::new()
        .schema_type(Type::Integer)
        .enum_values(Some([0, 1]))
        .description(Some("0 = pre-key, 1 = normal"))
        .build()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct OutgoingEnvelope {
    pub recipient_device_id: Uuid,
    /// 0 = pre-key, 1 = normal.
    #[cfg_attr(feature = "openapi", schema(schema_with = olm_message_type))]
    pub olm_message_type: u8,
    /// Standard base64 of the Olm message bytes.
    pub ciphertext: String,
}

/// `POST /v1/conversations/{id}/messages` body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SendMessageRequest {
    pub client_message_id: Uuid,
    pub envelopes: Vec<OutgoingEnvelope>,
}

/// `POST /v1/conversations/{id}/messages` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SendMessageResponse {
    pub accepted: i64,
    /// Member devices that were not addressed; encrypt for them and resend.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_devices: Vec<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct InboxEnvelope {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub sender_user_id: Uuid,
    pub sender_device_id: Uuid,
    pub sender_identity_key: String,
    /// Always `olm.v1`.
    pub algorithm: String,
    #[cfg_attr(feature = "openapi", schema(schema_with = olm_message_type))]
    pub olm_message_type: u8,
    /// Standard base64 of the Olm message bytes.
    pub ciphertext: String,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct InboxPage {
    pub items: Vec<InboxEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// `POST /v1/inbox/ack` body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct InboxAck {
    pub ids: Vec<Uuid>,
}

/// Limits from 05-social §4.4.
pub const MAX_ENVELOPES_PER_SEND: usize = 64;
pub const MAX_ENVELOPE_CIPHERTEXT: usize = 64 * 1024;
pub const MAX_PARTY_MEMBERS: usize = 16;

// ---------------------------------------------------------------------------------------
// Invites
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum InviteState {
    Pending,
    Accepted,
    Installing,
    Ready,
    Joined,
    Declined,
    Cancelled,
    Expired,
    Failed,
}

impl InviteState {
    pub const ALL: [InviteState; 9] = [
        InviteState::Pending,
        InviteState::Accepted,
        InviteState::Installing,
        InviteState::Ready,
        InviteState::Joined,
        InviteState::Declined,
        InviteState::Cancelled,
        InviteState::Expired,
        InviteState::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            InviteState::Pending => "pending",
            InviteState::Accepted => "accepted",
            InviteState::Installing => "installing",
            InviteState::Ready => "ready",
            InviteState::Joined => "joined",
            InviteState::Declined => "declined",
            InviteState::Cancelled => "cancelled",
            InviteState::Expired => "expired",
            InviteState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Still in play (counts toward "one active invite per sender, invitee and package").
    pub fn is_active(self) -> bool {
        matches!(
            self,
            InviteState::Pending
                | InviteState::Accepted
                | InviteState::Installing
                | InviteState::Ready
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum InviteFailure {
    NoBuildForPlatform,
    InstallFailed,
    InsufficientSpace,
    CancelledByUser,
}

impl InviteFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            InviteFailure::NoBuildForPlatform => "no_build_for_platform",
            InviteFailure::InstallFailed => "install_failed",
            InviteFailure::InsufficientSpace => "insufficient_space",
            InviteFailure::CancelledByUser => "cancelled_by_user",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "no_build_for_platform" => InviteFailure::NoBuildForPlatform,
            "install_failed" => InviteFailure::InstallFailed,
            "insufficient_space" => InviteFailure::InsufficientSpace,
            "cancelled_by_user" => InviteFailure::CancelledByUser,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Invite {
    pub id: Uuid,
    pub from: UserPublic,
    pub to: UserPublic,
    pub package: PackageSummary,
    pub state: InviteState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Documented as a plain string so new reasons stay compatible with older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub failure_reason: Option<InviteFailure>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub updated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub expires_at: OffsetDateTime,
}

/// `GET /v1/invites` response.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct InviteList {
    pub items: Vec<Invite>,
}

/// `POST /v1/invites` body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct InviteCreate {
    pub to_user_id: Uuid,
    pub package_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// States the invitee reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum InviteReport {
    Installing,
    Ready,
    Joined,
    Failed,
}

impl From<InviteReport> for InviteState {
    fn from(r: InviteReport) -> Self {
        match r {
            InviteReport::Installing => InviteState::Installing,
            InviteReport::Ready => InviteState::Ready,
            InviteReport::Joined => InviteState::Joined,
            InviteReport::Failed => InviteState::Failed,
        }
    }
}

/// `POST /v1/invites/{id}/status` body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct InviteStatusUpdate {
    pub state: InviteReport,
    /// Only with `installing`, 0..=1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    /// Only with `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<InviteFailure>,
}

/// Grammar for join secrets (01-security §7): checked by the invitee before substitution.
pub fn is_valid_join_secret(s: &str) -> bool {
    (1..=256).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".:_-[]".contains(&b))
}

// ---------------------------------------------------------------------------------------
// Encodings shared by the server and the launcher
// ---------------------------------------------------------------------------------------

/// `true` when `s` is unpadded standard base64 of exactly `len` characters.
pub fn is_unpadded_b64(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

/// Curve25519 / Ed25519 public keys: 32 bytes → 43 unpadded base64 chars.
pub const PUBLIC_KEY_B64_LEN: usize = 43;
/// Ed25519 signatures: 64 bytes → 86 unpadded base64 chars.
pub const SIGNATURE_B64_LEN: usize = 86;

/// The exact strings signed by a device's Ed25519 key (05-social-notes §2.1). The
/// server and every launcher use these functions, so the bytes can never drift apart.
pub mod canonical {
    use uuid::Uuid;

    pub const DEVICE_KEYS_TYPE: &str = "vgames.device_keys/1";
    pub const ONE_TIME_KEY_TYPE: &str = "vgames.otk/1";

    /// Canonical JSON of a device's identity keys, bound to the user and the server.
    pub fn device_keys(
        identity_key: &str,
        server_id: Uuid,
        signing_key: &str,
        user_id: Uuid,
    ) -> String {
        // Keys in byte order: identity_key < server_id < signing_key < type < user_id.
        format!(
            "{{\"identity_key\":{},\"server_id\":{},\"signing_key\":{},\"type\":{},\"user_id\":{}}}",
            string(identity_key),
            string(&server_id.hyphenated().to_string()),
            string(signing_key),
            string(DEVICE_KEYS_TYPE),
            string(&user_id.hyphenated().to_string()),
        )
    }

    /// Canonical JSON of a one-time or fallback key.
    pub fn one_time_key(fallback: bool, key: &str, key_id: &str) -> String {
        // fallback < key < key_id < type.
        format!(
            "{{\"fallback\":{},\"key\":{},\"key_id\":{},\"type\":{}}}",
            if fallback { "true" } else { "false" },
            string(key),
            string(key_id),
            string(ONE_TIME_KEY_TYPE),
        )
    }

    /// A JSON string literal with minimal escaping (RFC 8259 §7).
    fn string(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn device_keys_are_sorted_compact_json() {
            let user = Uuid::parse_str("01920000-0000-7000-8000-000000000001").unwrap();
            let server = Uuid::parse_str("01920000-0000-7000-8000-00000000abcd").unwrap();
            let s = device_keys(
                "I".repeat(43).as_str(),
                server,
                "S".repeat(43).as_str(),
                user,
            );
            assert_eq!(
                s,
                format!(
                    "{{\"identity_key\":\"{}\",\"server_id\":\"01920000-0000-7000-8000-00000000abcd\",\"signing_key\":\"{}\",\"type\":\"vgames.device_keys/1\",\"user_id\":\"01920000-0000-7000-8000-000000000001\"}}",
                    "I".repeat(43),
                    "S".repeat(43)
                )
            );
            // It is valid JSON whose keys are in sorted order.
            let v: serde_json::Value = serde_json::from_str(&s).unwrap();
            let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
            let mut sorted = keys.clone();
            sorted.sort();
            assert_eq!(keys, sorted);
        }

        #[test]
        fn one_time_key_json() {
            assert_eq!(
                one_time_key(true, "k", "AAAAAAAAAAE"),
                r#"{"fallback":true,"key":"k","key_id":"AAAAAAAAAAE","type":"vgames.otk/1"}"#
            );
        }

        #[test]
        fn escaping_is_minimal_and_matches_serde_json() {
            for s in ["plain", "quo\"te", "back\\slash", "nl\nx", "\u{1}ctl", "é"] {
                assert_eq!(string(s), serde_json::to_string(s).unwrap());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friend_codes_normalize_like_crockford() {
        assert_eq!(
            normalize_friend_code("7k2m-q9xd").as_deref(),
            Some("7K2MQ9XD")
        );
        assert_eq!(
            normalize_friend_code("ol1i 2345").as_deref(),
            Some("01112345")
        );
        assert_eq!(
            normalize_friend_code("7K2MQ9XU"),
            None,
            "U is not in the alphabet"
        );
        assert_eq!(normalize_friend_code("7K2MQ9X"), None);
        assert_eq!(normalize_friend_code("7K2MQ9XDD"), None);
        assert_eq!(normalize_friend_code("7K2MQ9XÉ"), None);
        assert!(is_friend_code("0123456Z"));
        assert!(!is_friend_code("0123456z"));
    }

    #[test]
    fn join_secret_grammar() {
        assert!(is_valid_join_secret("192.168.1.20:27015"));
        assert!(is_valid_join_secret("[::1]:7777"));
        assert!(is_valid_join_secret("lobby_42-A"));
        assert!(!is_valid_join_secret(""));
        assert!(!is_valid_join_secret("a b"));
        assert!(!is_valid_join_secret("x;rm"));
        assert!(!is_valid_join_secret("--flag=\"x\""));
        assert!(!is_valid_join_secret(&"a".repeat(257)));
        assert!(is_valid_join_secret(&"a".repeat(256)));
    }

    #[test]
    fn friend_request_bodies() {
        let by_user: FriendRequestCreate =
            serde_json::from_str(r#"{"user_id":"01920000-0000-7000-8000-000000000001"}"#).unwrap();
        assert!(matches!(by_user, FriendRequestCreate::User(_)));
        let by_code: FriendRequestCreate =
            serde_json::from_str(r#"{"friend_code":"7K2MQ9XD"}"#).unwrap();
        assert!(matches!(by_code, FriendRequestCreate::Code(_)));
        assert!(
            serde_json::from_str::<FriendRequestCreate>(
                r#"{"friend_code":"x","user_id":"01920000-0000-7000-8000-000000000001"}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<FriendRequestCreate>(r#"{}"#).is_err());
    }

    #[test]
    fn conversation_create_is_tagged_by_kind() {
        let c: ConversationCreate = serde_json::from_str(
            r#"{"kind":"party","user_ids":["01920000-0000-7000-8000-000000000001"]}"#,
        )
        .unwrap();
        assert!(matches!(c, ConversationCreate::Party { user_ids } if user_ids.len() == 1));
        assert!(
            serde_json::from_str::<ConversationCreate>(
                r#"{"kind":"direct","user_id":"01920000-0000-7000-8000-000000000001","x":1}"#
            )
            .is_err()
        );
    }
}
