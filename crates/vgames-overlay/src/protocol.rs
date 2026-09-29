//! The broker ⇄ renderer protocol (05-social §6.1).
//!
//! Loopback TCP, one connection per game. Every frame is a little-endian `u32` length
//! followed by a [postcard](https://docs.rs/postcard) payload of at most [`MAX_FRAME`] bytes.
//! The renderer speaks first with [`ToBroker::Hello`] (protocol version and the 32-byte
//! session token from `VGAMES_OVERLAY_TOKEN`); the broker answers [`ToRenderer::Welcome`]
//! or closes. Both sides send [`ToBroker::Heartbeat`] / [`ToRenderer::Heartbeat`] every
//! [`HEARTBEAT_EVERY`] and drop a link silent for [`DEAD_AFTER`].
//!
//! Both sides treat every inbound frame as untrusted: the length is checked before anything
//! is allocated, enums are closed (an unknown variant is a decode error), and every string
//! and list has a cap ([`limits`]) enforced by [`ToBroker::validate`] / [`View::validate`].

use std::io::{self, Read, Write};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Bumped on any incompatible change; the broker refuses other versions.
pub const PROTOCOL_VERSION: u16 = 1;
/// Largest payload in either direction.
pub const MAX_FRAME: usize = 64 * 1024;
/// Session token length (`VGAMES_OVERLAY_TOKEN` is its lowercase hex).
pub const TOKEN_LEN: usize = 32;
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);
pub const DEAD_AFTER: Duration = Duration::from_secs(15);

/// Environment variables the launcher sets for a game started with the overlay.
pub mod env {
    /// `1`: gates the Vulkan implicit layer (`enable_environment`).
    pub const ENABLED: &str = "VGAMES_OVERLAY";
    /// `127.0.0.1:<port>`.
    pub const ENDPOINT: &str = "VGAMES_OVERLAY_ENDPOINT";
    /// 64 lowercase hex characters.
    pub const TOKEN: &str = "VGAMES_OVERLAY_TOKEN";
}

/// Caps on view-model content (characters for strings).
pub mod limits {
    pub const TOASTS: usize = 4;
    pub const FRIENDS: usize = 50;
    pub const INVITES: usize = 20;
    pub const MESSAGES: usize = 20;
    pub const NAME: usize = 64;
    pub const TITLE: usize = 128;
    pub const TEXT: usize = 280;
    pub const REPLY: usize = 500;
}

/// What renders the overlay in the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RendererKind {
    D3d9,
    D3d11,
    D3d12,
    OpenGl,
    Vulkan,
    /// The launcher's own window (macOS panel, fallback window): not in the game.
    Window,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToastKind {
    Invite,
    Message,
    FriendOnline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Online,
    Away,
    InGame,
    Offline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Toast {
    pub id: Uuid,
    pub kind: ToastKind,
    pub title: String,
    pub body: String,
    /// Seconds the toast stays up (the renderer counts from receipt).
    pub ttl_secs: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FriendOnline {
    pub user_id: Uuid,
    pub name: String,
    pub status: Status,
    pub playing: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInvite {
    pub invite_id: Uuid,
    pub from: String,
    pub package_title: String,
    pub state: InviteState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentMessage {
    pub conversation_id: Uuid,
    pub from: String,
    pub text: String,
    /// Unix seconds.
    pub sent_at: i64,
}

/// Everything the renderer shows. Sent whole on every change (small, bounded).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub toasts: Vec<Toast>,
    pub friends_online: Vec<FriendOnline>,
    pub invites: Vec<PendingInvite>,
    pub recent_messages: Vec<RecentMessage>,
}

/// A user action in the overlay (closed set).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    AcceptInvite { invite_id: Uuid },
    DeclineInvite { invite_id: Uuid },
    QuickReply { conversation_id: Uuid, text: String },
    OpenLauncher,
    ClosePanel,
}

/// Renderer → broker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToBroker {
    Hello {
        version: u16,
        token: [u8; TOKEN_LEN],
        renderer: RendererKind,
    },
    Heartbeat,
    Action(Action),
}

/// Broker → renderer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToRenderer {
    Welcome {
        version: u16,
    },
    Heartbeat,
    View(View),
    /// Open or close the panel (hotkey or Guide/PS hold seen by the launcher).
    Panel {
        open: bool,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame of {0} bytes exceeds the limit")]
    TooLarge(usize),
    #[error("empty frame")]
    Empty,
    #[error("malformed frame")]
    Malformed,
    #[error("frame content out of bounds: {0}")]
    OutOfBounds(&'static str),
    #[error(transparent)]
    Io(#[from] io::Error),
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

fn check_text(s: &str, max: usize, what: &'static str) -> Result<(), FrameError> {
    if chars(s) > max || s.chars().any(|c| c.is_control() && c != '\n') {
        return Err(FrameError::OutOfBounds(what));
    }
    Ok(())
}

impl View {
    /// Checks every cap (the renderer runs this on each view it receives).
    pub fn validate(&self) -> Result<(), FrameError> {
        use limits::*;
        if self.toasts.len() > TOASTS
            || self.friends_online.len() > FRIENDS
            || self.invites.len() > INVITES
            || self.recent_messages.len() > MESSAGES
        {
            return Err(FrameError::OutOfBounds("list"));
        }
        for t in &self.toasts {
            check_text(&t.title, TITLE, "toast title")?;
            check_text(&t.body, TEXT, "toast body")?;
        }
        for f in &self.friends_online {
            check_text(&f.name, NAME, "friend name")?;
            if let Some(p) = &f.playing {
                check_text(p, TITLE, "playing")?;
            }
        }
        for i in &self.invites {
            check_text(&i.from, NAME, "invite from")?;
            check_text(&i.package_title, TITLE, "package title")?;
        }
        for m in &self.recent_messages {
            check_text(&m.from, NAME, "message from")?;
            check_text(&m.text, TEXT, "message text")?;
        }
        Ok(())
    }
}

impl ToBroker {
    /// Checks caps on renderer input (the broker runs this on every frame).
    pub fn validate(&self) -> Result<(), FrameError> {
        match self {
            ToBroker::Action(Action::QuickReply { text, .. }) => {
                if text.trim().is_empty() {
                    return Err(FrameError::OutOfBounds("empty reply"));
                }
                check_text(text, limits::REPLY, "reply")
            }
            _ => Ok(()),
        }
    }
}

impl ToRenderer {
    pub fn validate(&self) -> Result<(), FrameError> {
        match self {
            ToRenderer::View(v) => v.validate(),
            _ => Ok(()),
        }
    }
}

/// Truncates `s` to `max` characters and replaces control characters (for building views).
pub fn clip(s: &str, max: usize) -> String {
    s.chars()
        .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
        .take(max)
        .collect()
}

/// Encodes one frame (header + payload).
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, FrameError> {
    let payload = postcard::to_allocvec(message).map_err(|_| FrameError::Malformed)?;
    if payload.len() > MAX_FRAME {
        return Err(FrameError::TooLarge(payload.len()));
    }
    let len = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge(payload.len()))?;
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Checks a frame header and returns the payload length.
pub fn frame_len(header: [u8; 4]) -> Result<usize, FrameError> {
    let len = usize::try_from(u32::from_le_bytes(header)).map_err(|_| FrameError::Malformed)?;
    if len == 0 {
        return Err(FrameError::Empty);
    }
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    Ok(len)
}

/// Decodes a payload; trailing bytes are an error.
pub fn decode<T: DeserializeOwned>(payload: &[u8]) -> Result<T, FrameError> {
    let (value, rest) = postcard::take_from_bytes(payload).map_err(|_| FrameError::Malformed)?;
    if !rest.is_empty() {
        return Err(FrameError::Malformed);
    }
    Ok(value)
}

/// Blocking read of one frame (the renderer side).
pub fn read_frame<T: DeserializeOwned>(r: &mut impl Read) -> Result<T, FrameError> {
    let mut header = [0u8; 4];
    r.read_exact(&mut header)?;
    let len = frame_len(header)?;
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    decode(&payload)
}

/// Blocking write of one frame.
pub fn write_frame<T: Serialize>(w: &mut impl Write, message: &T) -> Result<(), FrameError> {
    w.write_all(&encode(message)?)?;
    w.flush()?;
    Ok(())
}

/// Parses `VGAMES_OVERLAY_TOKEN` (64 hex characters).
pub fn parse_token(hex: &str) -> Option<[u8; TOKEN_LEN]> {
    let hex = hex.as_bytes();
    if hex.len() != TOKEN_LEN * 2 {
        return None;
    }
    let mut out = [0u8; TOKEN_LEN];
    let (pairs, _) = hex.as_chunks::<2>();
    for (slot, [hi, lo]) in out.iter_mut().zip(pairs) {
        *slot = (nibble(*hi)? << 4) | nibble(*lo)?;
    }
    Some(out)
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// Lowercase hex of a token (for `VGAMES_OVERLAY_TOKEN`).
pub fn token_hex(token: &[u8; TOKEN_LEN]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(TOKEN_LEN * 2);
    for b in token {
        for n in [b >> 4, b & 15] {
            s.push(char::from(HEX.get(usize::from(n)).copied().unwrap_or(b'0')));
        }
    }
    s
}

/// Constant-time token comparison.
pub fn token_eq(a: &[u8; TOKEN_LEN], b: &[u8; TOKEN_LEN]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use proptest::prelude::*;

    fn view() -> View {
        View {
            toasts: vec![Toast {
                id: Uuid::from_u128(1),
                kind: ToastKind::Invite,
                title: "Sam invites you".into(),
                body: "Arena".into(),
                ttl_secs: 8,
            }],
            friends_online: vec![FriendOnline {
                user_id: Uuid::from_u128(2),
                name: "sam".into(),
                status: Status::InGame,
                playing: Some("Arena".into()),
            }],
            invites: vec![],
            recent_messages: vec![RecentMessage {
                conversation_id: Uuid::from_u128(3),
                from: "sam".into(),
                text: "gg\nwp".into(),
                sent_at: 1_790_000_000,
            }],
        }
    }

    #[test]
    fn frames_round_trip() {
        let msgs = [
            ToRenderer::Welcome {
                version: PROTOCOL_VERSION,
            },
            ToRenderer::View(view()),
            ToRenderer::Panel { open: true },
            ToRenderer::Heartbeat,
        ];
        let mut wire = Vec::new();
        for m in &msgs {
            write_frame(&mut wire, m).unwrap();
        }
        let mut r = wire.as_slice();
        for m in &msgs {
            assert_eq!(&read_frame::<ToRenderer>(&mut r).unwrap(), m);
        }
        let hello = ToBroker::Hello {
            version: PROTOCOL_VERSION,
            token: [7; TOKEN_LEN],
            renderer: RendererKind::Vulkan,
        };
        let bytes = encode(&hello).unwrap();
        assert_eq!(decode::<ToBroker>(&bytes[4..]).unwrap(), hello);
    }

    #[test]
    fn oversized_empty_and_trailing_frames_are_refused_before_allocation() {
        let big = u32::try_from(MAX_FRAME + 1).unwrap().to_le_bytes();
        assert!(matches!(frame_len(big), Err(FrameError::TooLarge(_))));
        assert!(matches!(frame_len([0; 4]), Err(FrameError::Empty)));
        assert!(matches!(frame_len([255; 4]), Err(FrameError::TooLarge(_))));
        let mut bytes = encode(&ToBroker::Heartbeat).unwrap()[4..].to_vec();
        bytes.push(0);
        assert!(decode::<ToBroker>(&bytes).is_err());
        // An unknown enum variant is an error, not a default.
        assert!(decode::<ToBroker>(&[42]).is_err());
        // Encoding refuses oversized content too.
        let reply = ToBroker::Action(Action::QuickReply {
            conversation_id: Uuid::nil(),
            text: "x".repeat(MAX_FRAME + 10),
        });
        assert!(matches!(encode(&reply), Err(FrameError::TooLarge(_))));
    }

    #[test]
    fn caps_are_enforced() {
        assert!(view().validate().is_ok());
        let mut v = view();
        v.toasts = vec![v.toasts[0].clone(); limits::TOASTS + 1];
        assert!(v.validate().is_err());
        let mut v = view();
        v.recent_messages[0].text = "x".repeat(limits::TEXT + 1);
        assert!(v.validate().is_err());
        let mut v = view();
        v.friends_online[0].name = "a\u{1b}[31m".into();
        assert!(v.validate().is_err());
        let reply = |t: &str| {
            ToBroker::Action(Action::QuickReply {
                conversation_id: Uuid::nil(),
                text: t.into(),
            })
            .validate()
        };
        assert!(reply("gg").is_ok());
        assert!(reply("   ").is_err());
        assert!(reply(&"é".repeat(limits::REPLY)).is_ok());
        assert!(reply(&"é".repeat(limits::REPLY + 1)).is_err());
        assert_eq!(clip("a\u{7}b", 2), "a ");
    }

    #[test]
    fn tokens() {
        let t: [u8; TOKEN_LEN] = core::array::from_fn(|i| u8::try_from(i * 7 % 256).unwrap());
        let hex = token_hex(&t);
        assert_eq!(hex.len(), 64);
        assert_eq!(parse_token(&hex), Some(t));
        assert_eq!(parse_token(&hex.to_uppercase()), None);
        assert_eq!(parse_token(&hex[..62]), None);
        assert!(token_eq(&t, &t));
        let mut u = t;
        u[31] ^= 1;
        assert!(!token_eq(&t, &u));
    }

    proptest! {
        #[test]
        fn decoding_arbitrary_bytes_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = decode::<ToBroker>(&bytes).map(|m| m.validate());
            let _ = decode::<ToRenderer>(&bytes).map(|m| m.validate());
            let mut r = bytes.as_slice();
            let _ = read_frame::<ToBroker>(&mut r);
        }
    }
}
