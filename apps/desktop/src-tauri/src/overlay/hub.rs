//! Builds the overlay's view model from social events (05-social §6.3): toasts for invites,
//! messages and friends coming online (8 s), friends online, open incoming invites and the
//! last messages. The same state feeds the overlay window (`OverlayView`) and every in-game
//! renderer (`protocol::View`, clipped to the protocol's caps).
//!
//! Do not disturb suppresses message and friend toasts; invite toasts still show (there are no
//! favorite friends yet to narrow them further).

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use time::OffsetDateTime;
use tokio::sync::watch;
use uuid::Uuid;
use vgames_overlay::protocol::{self, limits};

use super::model::{
    OverlayFriend, OverlayInvite, OverlayMessage, OverlayToast, OverlayToastKind, OverlayView,
};
use crate::events::PackageRef;
use crate::social::model::{
    Conversation, DeviceNotice, FriendList, Invite, InviteDirection, InviteInstallReason,
    InviteState, Message, MessageBody, MessageStatus, Presence, PresenceStatus, SocialConnection,
    UserSummary, rfc3339,
};
use crate::social::service::SocialEvents;

/// How long a toast stays up.
pub const TOAST_TTL: Duration = Duration::from_secs(8);
/// Names kept for "from" labels before the map is reset.
const MAX_NAMES: usize = 4096;

struct State {
    friends: FriendList,
    names: HashMap<Uuid, String>,
    invites: HashMap<Uuid, Invite>,
    messages: VecDeque<OverlayMessage>,
    toasts: VecDeque<(OverlayToast, OffsetDateTime)>,
    panel_open: bool,
    do_not_disturb: bool,
}

/// The overlay's live state. Cheap to share; changes are published on a watch channel.
pub struct Hub {
    state: Mutex<State>,
    tx: watch::Sender<OverlayView>,
}

fn name_of(u: &UserSummary) -> String {
    u.display_name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| u.username.clone())
}

fn is_open(state: InviteState) -> bool {
    matches!(
        state,
        InviteState::Pending | InviteState::Accepted | InviteState::Installing | InviteState::Ready
    )
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                friends: FriendList::default(),
                names: HashMap::new(),
                invites: HashMap::new(),
                messages: VecDeque::new(),
                toasts: VecDeque::new(),
                panel_open: false,
                do_not_disturb: false,
            }),
            tx: watch::channel(OverlayView::default()).0,
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<OverlayView> {
        self.tx.subscribe()
    }

    pub fn current(&self) -> OverlayView {
        self.tx.borrow().clone()
    }

    fn update(&self, f: impl FnOnce(&mut State)) {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut s);
        let view = build(&mut s, OffsetDateTime::now_utc());
        self.tx.send_if_modified(|cur| {
            if *cur == view {
                false
            } else {
                *cur = view;
                true
            }
        });
    }

    pub fn set_do_not_disturb(&self, on: bool) {
        self.update(|s| s.do_not_disturb = on);
    }

    pub fn set_panel(&self, open: bool) {
        self.update(|s| s.panel_open = open);
    }

    pub fn toggle_panel(&self) -> bool {
        let mut open = false;
        self.update(|s| {
            s.panel_open = !s.panel_open;
            open = s.panel_open;
        });
        open
    }

    fn toast(s: &mut State, kind: OverlayToastKind, title: String, body: String) {
        if s.do_not_disturb && kind != OverlayToastKind::Invite {
            return;
        }
        let now = OffsetDateTime::now_utc();
        let expires = now + TOAST_TTL;
        s.toasts.push_back((
            OverlayToast {
                id: Uuid::now_v7(),
                kind,
                title: protocol::clip(&title, limits::TITLE),
                body: protocol::clip(&body, limits::TEXT),
                expires_at: rfc3339(expires),
            },
            expires,
        ));
        while s.toasts.len() > limits::TOASTS {
            s.toasts.pop_front();
        }
    }

    fn remember(s: &mut State, users: impl IntoIterator<Item = UserSummary>) {
        if s.names.len() > MAX_NAMES {
            s.names.clear();
        }
        for u in users {
            let n = name_of(&u);
            s.names.insert(u.id, n);
        }
    }
}

fn build(s: &mut State, now: OffsetDateTime) -> OverlayView {
    s.toasts.retain(|(_, exp)| *exp > now);
    let mut friends: Vec<OverlayFriend> = s
        .friends
        .friends
        .iter()
        .filter_map(|f| {
            let p = f.presence.as_ref()?;
            (p.status != PresenceStatus::Offline).then(|| OverlayFriend {
                user_id: f.user.id,
                name: protocol::clip(&name_of(&f.user), limits::NAME),
                status: p.status,
                playing: p
                    .package_title
                    .as_deref()
                    .map(|t| protocol::clip(t, limits::TITLE)),
            })
        })
        .collect();
    friends.sort_by_key(|f| f.name.to_lowercase());
    friends.truncate(limits::FRIENDS);
    let mut invites: Vec<&Invite> = s
        .invites
        .values()
        .filter(|i| i.direction == InviteDirection::Incoming && is_open(i.state))
        .collect();
    invites.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let invites = invites
        .into_iter()
        .take(limits::INVITES)
        .map(|i| OverlayInvite {
            invite_id: i.id,
            from: protocol::clip(&name_of(&i.from), limits::NAME),
            package_title: protocol::clip(&i.package.title, limits::TITLE),
            state: i.state,
        })
        .collect();
    OverlayView {
        visible_panel: s.panel_open,
        toasts: s.toasts.iter().map(|(t, _)| t.clone()).collect(),
        friends_online: friends,
        invites,
        recent_messages: s.messages.iter().cloned().collect(),
    }
}

/// The protocol view for in-game renderers (already clipped; validated before sending).
pub fn to_protocol(v: &OverlayView, now: OffsetDateTime) -> protocol::View {
    use protocol as p;
    let status = |s: PresenceStatus| match s {
        PresenceStatus::Online => p::Status::Online,
        PresenceStatus::Away => p::Status::Away,
        PresenceStatus::InGame => p::Status::InGame,
        PresenceStatus::Offline => p::Status::Offline,
    };
    let invite_state = |s: InviteState| match s {
        InviteState::Pending => p::InviteState::Pending,
        InviteState::Accepted => p::InviteState::Accepted,
        InviteState::Installing => p::InviteState::Installing,
        InviteState::Ready => p::InviteState::Ready,
        InviteState::Joined => p::InviteState::Joined,
        InviteState::Declined => p::InviteState::Declined,
        InviteState::Cancelled => p::InviteState::Cancelled,
        InviteState::Expired => p::InviteState::Expired,
        InviteState::Failed => p::InviteState::Failed,
    };
    let parse =
        |t: &str| OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339).ok();
    p::View {
        toasts: v
            .toasts
            .iter()
            .filter_map(|t| {
                let left = parse(&t.expires_at)? - now;
                let secs = u16::try_from(left.whole_seconds().max(1)).unwrap_or(8);
                Some(p::Toast {
                    id: t.id,
                    kind: match t.kind {
                        OverlayToastKind::Invite => p::ToastKind::Invite,
                        OverlayToastKind::Message => p::ToastKind::Message,
                        OverlayToastKind::FriendOnline => p::ToastKind::FriendOnline,
                    },
                    title: t.title.clone(),
                    body: t.body.clone(),
                    ttl_secs: secs,
                })
            })
            .collect(),
        friends_online: v
            .friends_online
            .iter()
            .map(|f| p::FriendOnline {
                user_id: f.user_id,
                name: f.name.clone(),
                status: status(f.status),
                playing: f.playing.clone(),
            })
            .collect(),
        invites: v
            .invites
            .iter()
            .map(|i| p::PendingInvite {
                invite_id: i.invite_id,
                from: i.from.clone(),
                package_title: i.package_title.clone(),
                state: invite_state(i.state),
            })
            .collect(),
        recent_messages: v
            .recent_messages
            .iter()
            .map(|m| p::RecentMessage {
                conversation_id: m.conversation_id,
                from: m.from.clone(),
                text: m.text.clone(),
                sent_at: parse(&m.sent_at).map_or(0, OffsetDateTime::unix_timestamp),
            })
            .collect(),
    }
}

impl SocialEvents for Hub {
    fn connection_changed(&self, _: &SocialConnection) {}

    fn friends_changed(&self, friends: &FriendList) {
        self.update(|s| {
            Self::remember(
                s,
                friends
                    .friends
                    .iter()
                    .chain(&friends.incoming)
                    .chain(&friends.outgoing)
                    .map(|f| f.user.clone()),
            );
            s.friends = friends.clone();
        });
    }

    fn presence_changed(&self, user_id: Uuid, presence: &Presence) {
        self.update(|s| {
            let Some(f) = s.friends.friends.iter_mut().find(|f| f.user.id == user_id) else {
                return;
            };
            let was_offline = f
                .presence
                .as_ref()
                .is_none_or(|p| p.status == PresenceStatus::Offline);
            f.presence = Some(presence.clone());
            if was_offline && presence.status != PresenceStatus::Offline {
                let name = name_of(&f.user);
                Self::toast(
                    s,
                    OverlayToastKind::FriendOnline,
                    name,
                    "is online".to_owned(),
                );
            }
        });
    }

    fn friend_request_received(&self, _: &UserSummary) {}

    fn conversations_changed(&self, conversations: &[Conversation]) {
        self.update(|s| {
            Self::remember(s, conversations.iter().flat_map(|c| c.members.clone()));
        });
    }

    fn message_received(&self, message: &Message) {
        let MessageBody::Text { text } = &message.body else {
            return;
        };
        if message.mine {
            return;
        }
        self.update(|s| {
            let from = s
                .names
                .get(&message.sender_user_id)
                .cloned()
                .unwrap_or_else(|| "A friend".to_owned());
            s.messages.push_back(OverlayMessage {
                conversation_id: message.conversation_id,
                from: protocol::clip(&from, limits::NAME),
                text: protocol::clip(text, limits::TEXT),
                sent_at: message.sent_at.clone(),
            });
            while s.messages.len() > limits::MESSAGES {
                s.messages.pop_front();
            }
            Self::toast(s, OverlayToastKind::Message, from, text.clone());
        });
    }

    fn message_status_changed(&self, _: Uuid, _: Uuid, _: MessageStatus) {}
    fn typing(&self, _: Uuid, _: Uuid) {}
    fn device_notice(&self, _: Option<Uuid>, _: &DeviceNotice) {}

    fn invite_received(&self, invite: &Invite) {
        self.update(|s| {
            Self::remember(s, [invite.from.clone(), invite.to.clone()]);
            let title = format!("{} invites you to play", name_of(&invite.from));
            Self::toast(
                s,
                OverlayToastKind::Invite,
                title,
                invite.package.title.clone(),
            );
            s.invites.insert(invite.id, invite.clone());
        });
    }

    fn invite_changed(&self, invite: &Invite) {
        self.update(|s| {
            if is_open(invite.state) {
                s.invites.insert(invite.id, invite.clone());
            } else {
                s.invites.remove(&invite.id);
            }
        });
    }

    fn invite_install_requested(&self, _: Uuid, _: PackageRef, _: InviteInstallReason) {}
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use crate::social::model::{Friend, FriendState, InvitePackage};

    fn user(id: u128, name: &str) -> UserSummary {
        UserSummary {
            id: Uuid::from_u128(id),
            username: name.into(),
            display_name: None,
            avatar_url: None,
        }
    }

    fn presence(status: PresenceStatus, playing: Option<&str>) -> Presence {
        Presence {
            status,
            package_id: None,
            package_title: playing.map(Into::into),
            updated_at: None,
        }
    }

    fn invite(id: u128, state: InviteState) -> Invite {
        Invite {
            id: Uuid::from_u128(id),
            direction: InviteDirection::Incoming,
            from: user(5, "sam"),
            to: user(1, "me"),
            package: InvitePackage {
                id: Uuid::from_u128(77),
                title: "Arena".into(),
                cover_url: None,
            },
            state,
            progress: None,
            message: None,
            failure_reason: None,
            created_at: "2026-09-29T10:00:00Z".into(),
            updated_at: "2026-09-29T10:00:00Z".into(),
            expires_at: "2026-09-29T10:10:00Z".into(),
            has_join_secret: false,
        }
    }

    fn message(from: u128, text: &str) -> Message {
        Message {
            id: Uuid::now_v7(),
            conversation_id: Uuid::from_u128(9),
            sender_user_id: Uuid::from_u128(from),
            mine: false,
            body: MessageBody::Text { text: text.into() },
            sent_at: "2026-09-29T10:00:00Z".into(),
            received_at: None,
            status: MessageStatus::Received,
        }
    }

    #[test]
    fn events_build_a_bounded_view_and_toasts() {
        let hub = Hub::new();
        hub.friends_changed(&FriendList {
            friends: vec![
                Friend {
                    user: user(5, "sam"),
                    state: FriendState::Accepted,
                    presence: Some(presence(PresenceStatus::Offline, None)),
                    since: None,
                },
                Friend {
                    user: user(6, "ana"),
                    state: FriendState::Accepted,
                    presence: Some(presence(PresenceStatus::InGame, Some("Racer"))),
                    since: None,
                },
            ],
            incoming: vec![],
            outgoing: vec![],
        });
        let v = hub.current();
        assert_eq!(v.friends_online.len(), 1);
        assert_eq!(v.friends_online[0].playing.as_deref(), Some("Racer"));
        assert!(v.toasts.is_empty());

        // Sam comes online: a toast and a row.
        hub.presence_changed(Uuid::from_u128(5), &presence(PresenceStatus::Online, None));
        let v = hub.current();
        assert_eq!(v.friends_online.len(), 2);
        assert_eq!(v.toasts.len(), 1);
        assert_eq!(v.toasts[0].kind, OverlayToastKind::FriendOnline);

        // An invite: toast + open invite; declined → gone from the list.
        hub.invite_received(&invite(20, InviteState::Pending));
        let v = hub.current();
        assert_eq!(v.invites.len(), 1);
        assert_eq!(v.toasts.last().unwrap().title, "sam invites you to play");
        hub.invite_changed(&invite(20, InviteState::Declined));
        assert!(hub.current().invites.is_empty());

        // Messages: named, clipped, bounded, own messages ignored.
        hub.message_received(&message(5, &"x".repeat(1000)));
        let v = hub.current();
        assert_eq!(v.recent_messages[0].from, "sam");
        assert_eq!(v.recent_messages[0].text.chars().count(), limits::TEXT);
        for i in 0..30 {
            hub.message_received(&message(6, &format!("m{i}")));
        }
        let mut mine = message(6, "mine");
        mine.mine = true;
        hub.message_received(&mine);
        let v = hub.current();
        assert_eq!(v.recent_messages.len(), limits::MESSAGES);
        assert_eq!(v.recent_messages.last().unwrap().text, "m29");
        assert_eq!(v.toasts.len(), limits::TOASTS);

        // The protocol view passes the renderer's own checks.
        let p = to_protocol(&v, OffsetDateTime::now_utc());
        p.validate().unwrap();
        assert_eq!(p.recent_messages.len(), limits::MESSAGES);
    }

    #[test]
    fn do_not_disturb_keeps_only_invite_toasts() {
        let hub = Hub::new();
        hub.set_do_not_disturb(true);
        hub.message_received(&message(5, "hi"));
        assert!(hub.current().toasts.is_empty());
        assert_eq!(hub.current().recent_messages.len(), 1);
        hub.invite_received(&invite(21, InviteState::Pending));
        assert_eq!(hub.current().toasts.len(), 1);
        assert!(hub.toggle_panel());
        assert!(hub.current().visible_panel);
        assert!(!hub.toggle_panel());
    }
}
