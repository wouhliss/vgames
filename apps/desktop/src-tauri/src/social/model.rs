//! Types the social commands and events hand to the UI (05-social-notes §5–§7).
//!
//! These are the tauri-specta payloads: snake_case fields, enums tagged by `kind`,
//! timestamps as RFC 3339 strings. Nothing here carries a key, a token or a join secret.

use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

/// Why a social command failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SocialError {
    #[error("not signed in")]
    NotSignedIn,
    #[error("the server could not be reached")]
    Offline,
    #[error("not found")]
    NotFound,
    #[error("invalid {field}: {message}")]
    InvalidInput { field: String, message: String },
    #[error("rate limited")]
    RateLimited { retry_after_seconds: u32 },
    #[error("the friend code is unknown, expired or used")]
    CodeInvalid,
    #[error("limit reached")]
    LimitReached { limit: SocialLimit },
    #[error("conflict: {code}")]
    Conflict { code: String, message: String },
    #[error("a contact's device key changed")]
    KeyChanged {
        user_id: Uuid,
        device_ids: Vec<Uuid>,
    },
    #[error("server error {code}")]
    Server { code: String, message: String },
    #[error("internal error: {detail}")]
    Internal { detail: String },
}

impl SocialError {
    pub fn invalid(field: &str, message: &str) -> Self {
        Self::InvalidInput {
            field: field.to_owned(),
            message: message.to_owned(),
        }
    }

    /// Logs the details and returns a generic internal error.
    pub fn internal(context: &str, error: &dyn std::error::Error) -> Self {
        tracing::error!(error = %crate::error::DisplayChain(error), "{context}");
        Self::Internal {
            detail: context.to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SocialLimit {
    Friends,
    PendingRequests,
    PartyMembers,
}

// ---- people -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct UserSummary {
    pub id: Uuid,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

impl From<vgames_proto::auth::UserPublic> for UserSummary {
    fn from(u: vgames_proto::auth::UserPublic) -> Self {
        Self {
            id: u.id,
            username: u.username,
            display_name: u.display_name,
            avatar_url: u.avatar_url,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PresenceStatus {
    Online,
    Away,
    InGame,
    Offline,
}

impl From<vgames_proto::social::PresenceStatus> for PresenceStatus {
    fn from(s: vgames_proto::social::PresenceStatus) -> Self {
        use vgames_proto::social::PresenceStatus as P;
        match s {
            P::Online => Self::Online,
            P::Away => Self::Away,
            P::InGame => Self::InGame,
            P::Offline => Self::Offline,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct Presence {
    pub status: PresenceStatus,
    pub package_id: Option<Uuid>,
    pub package_title: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum FriendState {
    Accepted,
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct Friend {
    pub user: UserSummary,
    pub state: FriendState,
    pub presence: Option<Presence>,
    pub since: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct FriendList {
    pub friends: Vec<Friend>,
    pub incoming: Vec<Friend>,
    pub outgoing: Vec<Friend>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct FriendCode {
    pub code: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FriendTarget {
    Code { code: String },
    User { user_id: Uuid },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct BlockedUser {
    pub user_id: Uuid,
    pub username: Option<String>,
    pub blocked_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SocialConnectionState {
    SignedOut,
    Connecting,
    Connected,
    Reconnecting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SocialConnection {
    pub server_id: Option<Uuid>,
    pub state: SocialConnectionState,
    pub retry_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SocialSettings {
    pub show_current_game: bool,
    pub do_not_disturb: bool,
    pub overlay_enabled: bool,
    pub overlay_hotkey: String,
}

impl Default for SocialSettings {
    fn default() -> Self {
        Self {
            show_current_game: true,
            do_not_disturb: false,
            overlay_enabled: true,
            overlay_hotkey: "Shift+F3".to_owned(),
        }
    }
}

// ---- messaging --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    Direct,
    Party,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct Conversation {
    pub id: Uuid,
    pub kind: ConversationKind,
    pub members: Vec<UserSummary>,
    pub last_message: Option<Message>,
    pub unread: u32,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Pending,
    Sent,
    Failed,
    Received,
}

impl MessageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sent => "sent",
            Self::Failed => "failed",
            Self::Received => "received",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "sent" => Self::Sent,
            "failed" => Self::Failed,
            _ => Self::Received,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeviceNotice {
    NewDevice {
        user_id: Uuid,
        device_id: Uuid,
        device_name: String,
    },
    KeyChanged {
        user_id: Uuid,
        device_id: Uuid,
    },
    DeviceRevoked {
        user_id: Uuid,
        device_id: Uuid,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageBody {
    Text { text: String },
    InviteJoin { invite_id: Uuid },
    Notice { notice: DeviceNotice },
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct Message {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub sender_user_id: Uuid,
    pub mine: bool,
    pub body: MessageBody,
    pub sent_at: String,
    pub received_at: Option<String>,
    pub status: MessageStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ContactDeviceState {
    Trusted,
    New,
    KeyChanged,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ContactDevice {
    pub device_id: Uuid,
    pub display_name: Option<String>,
    pub first_seen_at: String,
    pub key_fingerprint: String,
    pub state: ContactDeviceState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ContactSecurity {
    pub user_id: Uuid,
    pub verified: bool,
    pub needs_reverification: bool,
    pub safety_number: String,
    pub safety_number_groups: Vec<String>,
    pub devices: Vec<ContactDevice>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DevicePlatform {
    Windows,
    Linux,
    Macos,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MyDevice {
    pub id: Uuid,
    pub display_name: String,
    pub platform: DevicePlatform,
    pub current: bool,
    pub created_at: String,
    pub last_seen_at: Option<String>,
}

// ---- invites ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
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

impl From<vgames_proto::social::InviteState> for InviteState {
    fn from(s: vgames_proto::social::InviteState) -> Self {
        use vgames_proto::social::InviteState as S;
        match s {
            S::Pending => Self::Pending,
            S::Accepted => Self::Accepted,
            S::Installing => Self::Installing,
            S::Ready => Self::Ready,
            S::Joined => Self::Joined,
            S::Declined => Self::Declined,
            S::Cancelled => Self::Cancelled,
            S::Expired => Self::Expired,
            S::Failed => Self::Failed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum InviteFailure {
    NoBuildForPlatform,
    InstallFailed,
    InsufficientSpace,
    CancelledByUser,
}

impl From<vgames_proto::social::InviteFailure> for InviteFailure {
    fn from(f: vgames_proto::social::InviteFailure) -> Self {
        use vgames_proto::social::InviteFailure as F;
        match f {
            F::NoBuildForPlatform => Self::NoBuildForPlatform,
            F::InstallFailed => Self::InstallFailed,
            F::InsufficientSpace => Self::InsufficientSpace,
            F::CancelledByUser => Self::CancelledByUser,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum InviteDirection {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct InvitePackage {
    pub id: Uuid,
    pub title: String,
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct Invite {
    pub id: Uuid,
    pub direction: InviteDirection,
    pub from: UserSummary,
    pub to: UserSummary,
    pub package: InvitePackage,
    pub state: InviteState,
    pub progress: Option<f64>,
    pub message: Option<String>,
    pub failure_reason: Option<InviteFailure>,
    pub created_at: String,
    pub updated_at: String,
    pub expires_at: String,
    pub has_join_secret: bool,
}

/// Why the invite flow opens the install dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum InviteInstallReason {
    Missing,
    Outdated,
}

impl Invite {
    /// The UI model of a server invite, seen by `me`. `has_join_secret` is known locally.
    pub fn from_proto(
        i: vgames_proto::social::Invite,
        me: Uuid,
        server: Uuid,
        has_join_secret: bool,
    ) -> Self {
        let direction = if i.from.id == me {
            InviteDirection::Outgoing
        } else {
            InviteDirection::Incoming
        };
        let cover_url = i
            .package
            .cover
            .as_ref()
            .and_then(|a| crate::images::ImageKey::for_asset(server, &a.id.to_string()).ok())
            .map(|k| k.url());
        Self {
            id: i.id,
            direction,
            from: i.from.into(),
            to: i.to.into(),
            package: InvitePackage {
                id: i.package.id,
                title: i.package.title,
                cover_url,
            },
            state: i.state.into(),
            progress: i.progress,
            message: i.message,
            failure_reason: i.failure_reason.map(Into::into),
            created_at: rfc3339(i.created_at),
            updated_at: rfc3339(i.updated_at),
            expires_at: rfc3339(i.expires_at),
            has_join_secret: direction == InviteDirection::Outgoing && has_join_secret,
        }
    }
}

/// RFC 3339 rendering for UI payloads.
pub fn rfc3339(t: time::OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Unix seconds (the local database's time format) to RFC 3339.
pub fn unix_rfc3339(secs: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(secs)
        .map(rfc3339)
        .unwrap_or_default()
}
