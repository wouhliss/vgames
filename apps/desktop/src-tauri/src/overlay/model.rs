//! The overlay window's view model and actions (05-social-notes §7), as tauri-specta types.
//! The in-game renderer gets the same content as `vgames_overlay::protocol::View`.

use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

use crate::social::model::{InviteState, PresenceStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum OverlayToastKind {
    Invite,
    Message,
    FriendOnline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct OverlayToast {
    pub id: Uuid,
    pub kind: OverlayToastKind,
    pub title: String,
    pub body: String,
    /// RFC 3339.
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct OverlayFriend {
    pub user_id: Uuid,
    pub name: String,
    pub status: PresenceStatus,
    pub playing: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct OverlayInvite {
    pub invite_id: Uuid,
    pub from: String,
    pub package_title: String,
    pub state: InviteState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct OverlayMessage {
    pub conversation_id: Uuid,
    pub from: String,
    pub text: String,
    /// RFC 3339.
    pub sent_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct OverlayView {
    pub visible_panel: bool,
    pub toasts: Vec<OverlayToast>,
    pub friends_online: Vec<OverlayFriend>,
    pub invites: Vec<OverlayInvite>,
    pub recent_messages: Vec<OverlayMessage>,
}

/// What the overlay (window or in-game renderer) asks the launcher to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlayAction {
    AcceptInvite { invite_id: Uuid },
    DeclineInvite { invite_id: Uuid },
    QuickReply { conversation_id: Uuid, text: String },
    OpenLauncher,
    ClosePanel,
}

impl From<vgames_overlay::protocol::Action> for OverlayAction {
    fn from(a: vgames_overlay::protocol::Action) -> Self {
        use vgames_overlay::protocol::Action as A;
        match a {
            A::AcceptInvite { invite_id } => Self::AcceptInvite { invite_id },
            A::DeclineInvite { invite_id } => Self::DeclineInvite { invite_id },
            A::QuickReply {
                conversation_id,
                text,
            } => Self::QuickReply {
                conversation_id,
                text,
            },
            A::OpenLauncher => Self::OpenLauncher,
            A::ClosePanel => Self::ClosePanel,
        }
    }
}

/// The per-package "In-game overlay" switch and its safety valve (Settings → Overlay).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PackageOverlay {
    pub package: crate::events::PackageRef,
    pub title: String,
    pub enabled: bool,
    /// RFC 3339; set when the launcher turned the overlay off after two quick abnormal exits.
    pub disabled_by_safety_valve_at: Option<String>,
}
