//! CPU layout and rasterisation of the in-game overlay (05-social §6.1), shared by the
//! backends that copy pixels into the game's frame (Vulkan layer, GL preload).
//!
//! The overlay is a few opaque cards: toasts stacked at the top right, and the panel as a
//! column on the right edge. Each card is one [`Region`] of BGRA pixels the backend copies
//! into the swapchain image; nothing is blended, so no shader or pipeline state of the game
//! is touched. Text uses the public-domain 8×8 bitmap font, scaled with the frame height.
//! Server text is drawn as plain glyphs; characters the font lacks become `?`.

use font8x8::{BASIC_FONTS, LATIN_FONTS, UnicodeFonts as _};

use crate::protocol::{InviteState, Status, ToastKind, View};

/// A rectangle of the frame and its pixels (`0xAARRGGBB`, rows top to bottom).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

const BG: u32 = 0xFF1C_1D24;
const PANEL_BG: u32 = 0xFF15_161C;
const TEXT: u32 = 0xFFF2_F3F5;
const MUTED: u32 = 0xFFA9_ADB8;
const INVITE: u32 = 0xFF5B_8CFF;
const MESSAGE: u32 = 0xFF3F_C98A;
const ONLINE: u32 = 0xFFF2_B84B;

/// Frames narrower or shorter than this get no overlay.
const MIN_FRAME: u32 = 320;
const TOAST_WIDTH: u32 = 320;
const PANEL_WIDTH: u32 = 380;
const MARGIN: u32 = 12;
const PAD: u32 = 10;
const GAP: u32 = 8;
const ACCENT: u32 = 4;
const BODY_LINES: usize = 2;

/// Integer scale for the frame: 1 up to 900 px high, 2 up to 1800, then 3.
pub fn scale_for(frame_height: u32) -> u32 {
    (frame_height / 900 + 1).min(3)
}

/// The cards to draw, clipped to a `frame_width` × `frame_height` image.
pub fn compose(
    view: &View,
    alive: &[bool],
    panel_open: bool,
    frame_width: u32,
    frame_height: u32,
) -> Vec<Region> {
    if frame_width < MIN_FRAME || frame_height < MIN_FRAME {
        return Vec::new();
    }
    let s = scale_for(frame_height);
    if panel_open {
        return panel(view, s, frame_width, frame_height)
            .into_iter()
            .collect();
    }
    let width = (TOAST_WIDTH * s).min(frame_width - 2 * MARGIN);
    let x = frame_width - MARGIN - width;
    let mut y = MARGIN;
    let mut out = Vec::new();
    for (toast, _) in view
        .toasts
        .iter()
        .zip(alive.iter().chain(std::iter::repeat(&false)))
        .filter(|(_, a)| **a)
    {
        let accent = match toast.kind {
            ToastKind::Invite => INVITE,
            ToastKind::Message => MESSAGE,
            ToastKind::FriendOnline => ONLINE,
        };
        let mut c = Card::new(width, s, BG);
        c.accent(accent);
        c.line(&toast.title, TEXT);
        for l in wrap(&toast.body, c.columns(), BODY_LINES) {
            c.line(&l, MUTED);
        }
        let region = c.finish(x, y);
        if region.y + region.height > frame_height {
            break;
        }
        y += region.height + GAP * s;
        out.push(region);
    }
    out
}

fn panel(view: &View, s: u32, frame_width: u32, frame_height: u32) -> Option<Region> {
    let width = (PANEL_WIDTH * s).min(frame_width);
    let mut c = Card::new(width, s, PANEL_BG);
    c.max_height = Some(frame_height);
    c.line("vgames", TEXT);
    c.blank();
    c.line("Invites", INVITE);
    if view.invites.is_empty() {
        c.line("  none", MUTED);
    }
    for i in &view.invites {
        c.line(&format!("  {} - {}", i.from, i.package_title), TEXT);
        c.line(&format!("    {}", invite_state(i.state)), MUTED);
    }
    c.blank();
    c.line("Messages", MESSAGE);
    if view.recent_messages.is_empty() {
        c.line("  none", MUTED);
    }
    for m in &view.recent_messages {
        c.line(&format!("  {}: {}", m.from, m.text), TEXT);
    }
    c.blank();
    c.line("Friends online", ONLINE);
    if view.friends_online.is_empty() {
        c.line("  none", MUTED);
    }
    for f in &view.friends_online {
        let what = match (&f.playing, f.status) {
            (Some(p), _) => format!("  {} - {p}", f.name),
            (None, Status::Away) => format!("  {} (away)", f.name),
            (None, _) => format!("  {}", f.name),
        };
        c.line(&what, TEXT);
    }
    c.blank();
    c.line("Open vgames to reply or accept.", MUTED);
    // The column runs the full height of the frame.
    c.min_height = frame_height;
    Some(c.finish(frame_width - width, 0))
}

fn invite_state(s: InviteState) -> &'static str {
    match s {
        InviteState::Pending => "invited you",
        InviteState::Accepted => "accepted",
        InviteState::Installing => "installing",
        InviteState::Ready => "ready to join",
        InviteState::Joined => "joined",
        InviteState::Declined => "declined",
        InviteState::Cancelled => "cancelled",
        InviteState::Expired => "expired",
        InviteState::Failed => "failed",
    }
}

/// Splits `text` into at most `lines` lines of `columns` characters (the last one ends
/// with `...` when text was cut).
fn wrap(text: &str, columns: usize, lines: usize) -> Vec<String> {
    let columns = columns.max(4);
    let chars: Vec<char> = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut out: Vec<String> = chars.chunks(columns).map(|c| c.iter().collect()).collect();
    if out.len() > lines {
        out.truncate(lines);
        if let Some(last) = out.last_mut() {
            let keep: String = last.chars().take(columns - 3).collect();
            *last = format!("{keep}...");
        }
    }
    out
}

/// A card being laid out: lines of text on a background, grown downwards.
struct Card {
    width: u32,
    scale: u32,
    bg: u32,
    accent: Option<u32>,
    lines: Vec<(String, u32)>,
    min_height: u32,
    max_height: Option<u32>,
}

impl Card {
    fn new(width: u32, scale: u32, bg: u32) -> Self {
        Self {
            width,
            scale,
            bg,
            accent: None,
            lines: Vec::new(),
            min_height: 0,
            max_height: None,
        }
    }

    fn glyph(&self) -> u32 {
        8 * self.scale
    }

    fn line_height(&self) -> u32 {
        self.glyph() + 4 * self.scale
    }

    fn text_x(&self) -> u32 {
        PAD * self.scale + self.accent.map_or(0, |_| ACCENT * self.scale)
    }

    fn columns(&self) -> usize {
        let usable = self.width.saturating_sub(self.text_x() + PAD * self.scale);
        (usable / self.glyph()) as usize
    }

    fn accent(&mut self, color: u32) {
        self.accent = Some(color);
    }

    fn line(&mut self, text: &str, color: u32) {
        let columns = self.columns();
        let mut t: String = text.chars().take(columns).collect();
        if text.chars().count() > columns && columns > 3 {
            t = t.chars().take(columns - 3).collect::<String>() + "...";
        }
        self.lines.push((t, color));
    }

    fn blank(&mut self) {
        self.lines.push((String::new(), 0));
    }

    fn finish(self, x: u32, y: u32) -> Region {
        let s = self.scale;
        let lh = self.line_height();
        let mut height = (2 * PAD * s + lh * self.lines.len() as u32).max(self.min_height);
        if let Some(max) = self.max_height {
            height = height.min(max);
        }
        let mut px = Pixels {
            width: self.width,
            height,
            data: vec![self.bg; (self.width * height) as usize],
        };
        if let Some(a) = self.accent {
            px.fill(0, 0, ACCENT * s, height, a);
        }
        let tx = self.text_x();
        for (i, (text, color)) in self.lines.iter().enumerate() {
            let ty = PAD * s + lh * i as u32 + 2 * s;
            if ty + self.glyph() > height {
                break;
            }
            for (j, ch) in text.chars().enumerate() {
                px.glyph(tx + j as u32 * self.glyph(), ty, s, ch, *color);
            }
        }
        Region {
            x,
            y,
            width: self.width,
            height,
            pixels: px.data,
        }
    }
}

struct Pixels {
    width: u32,
    height: u32,
    data: Vec<u32>,
}

impl Pixels {
    fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        for yy in y..(y + h).min(self.height) {
            for xx in x..(x + w).min(self.width) {
                if let Some(p) = self.data.get_mut((yy * self.width + xx) as usize) {
                    *p = color;
                }
            }
        }
    }

    fn glyph(&mut self, x: u32, y: u32, scale: u32, ch: char, color: u32) {
        if ch == ' ' {
            return;
        }
        let rows = BASIC_FONTS
            .get(ch)
            .or_else(|| LATIN_FONTS.get(ch))
            .or_else(|| BASIC_FONTS.get('?'))
            .unwrap_or([0; 8]);
        for (r, bits) in rows.iter().enumerate() {
            for c in 0..8u32 {
                if bits & (1 << c) != 0 {
                    self.fill(x + c * scale, y + r as u32 * scale, scale, scale, color);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use uuid::Uuid;

    use super::*;
    use crate::protocol::{FriendOnline, PendingInvite, RecentMessage, Toast};

    fn toast(kind: ToastKind, body: &str) -> Toast {
        Toast {
            id: Uuid::now_v7(),
            kind,
            title: "Sam".into(),
            body: body.into(),
            ttl_secs: 8,
        }
    }

    fn inside(r: &Region, w: u32, h: u32) -> bool {
        r.x + r.width <= w && r.y + r.height <= h && r.pixels.len() == (r.width * r.height) as usize
    }

    #[test]
    fn live_toasts_stack_at_the_top_right() {
        let mut v = View::default();
        v.toasts.push(toast(ToastKind::Message, "gg"));
        v.toasts.push(toast(ToastKind::Invite, "expired one"));
        v.toasts
            .push(toast(ToastKind::FriendOnline, &"long ".repeat(100)));
        let r = compose(&v, &[true, false, true], false, 1920, 1080);
        assert_eq!(r.len(), 2);
        let s = scale_for(1080);
        assert_eq!(s, 2);
        for x in &r {
            assert!(inside(x, 1920, 1080));
            assert_eq!(x.x + x.width, 1920 - MARGIN);
            // Text was drawn (some pixels are neither background nor accent).
            assert!(x.pixels.contains(&TEXT));
        }
        assert!(r[1].y > r[0].y + r[0].height);
        // Accent colour by kind.
        assert_eq!(r[0].pixels[0], MESSAGE);
        assert_eq!(r[1].pixels[0], ONLINE);
        // Long bodies are cut to two lines: both toasts are the same height.
        assert_eq!(r[0].height, r[1].height - Card::new(1, s, 0).line_height());
    }

    #[test]
    fn nothing_for_tiny_frames_or_no_live_toasts() {
        let mut v = View::default();
        v.toasts.push(toast(ToastKind::Message, "gg"));
        assert!(compose(&v, &[true], false, 200, 1080).is_empty());
        assert!(compose(&v, &[false], false, 1920, 1080).is_empty());
        assert!(compose(&v, &[], false, 1920, 1080).is_empty());
        assert!(compose(&View::default(), &[], false, 1920, 1080).is_empty());
    }

    #[test]
    fn the_panel_is_a_full_height_column_and_clips_long_lists() {
        let mut v = View::default();
        for i in 0..50 {
            v.friends_online.push(FriendOnline {
                user_id: Uuid::now_v7(),
                name: format!("friend {i} with a long name ÿ ✓"),
                status: Status::InGame,
                playing: Some("Arena".into()),
            });
        }
        v.invites.push(PendingInvite {
            invite_id: Uuid::now_v7(),
            from: "Sam".into(),
            package_title: "Arena".into(),
            state: InviteState::Ready,
        });
        v.recent_messages.push(RecentMessage {
            conversation_id: Uuid::now_v7(),
            from: "Sam".into(),
            text: "\u{1b}[31m still text".into(),
            sent_at: 0,
        });
        for (w, h) in [(1280, 720), (3840, 2160), (400, 400)] {
            let r = compose(&v, &[], true, w, h);
            assert_eq!(r.len(), 1);
            assert!(inside(&r[0], w, h));
            assert_eq!(r[0].height, h);
            assert_eq!(r[0].x + r[0].width, w);
        }
    }

    #[test]
    fn wrapping_cuts_with_an_ellipsis() {
        assert_eq!(wrap("abcdefgh", 4, 2), vec!["abcd", "efgh"]);
        assert_eq!(wrap("abcdefghij", 4, 2), vec!["abcd", "e..."]);
        assert_eq!(wrap("a\nb", 10, 2), vec!["a b"]);
        assert!(wrap("", 10, 2).is_empty());
    }
}
