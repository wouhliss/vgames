//! Canonical pad state → Xbox 360 (XInput) report (07-controllers §5).
//!
//! The canonical state uses SDL's gamepad layout: face buttons by position
//! (south/east/west/north), sticks in −32768..=32767 with Y pointing down,
//! triggers in 0..=32767. A [`Mapper`] is built once per pad and session from
//! the user's [`Profile`]; [`Mapper::map`] then runs per input event with no
//! allocation.

use serde::{Deserialize, Serialize};

use crate::events::ControllerKind;

/// Buttons of the canonical (SDL gamepad) layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    South,
    East,
    West,
    North,
    Back,
    Guide,
    Start,
    LeftStick,
    RightStick,
    LeftShoulder,
    RightShoulder,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    /// DualSense touchpad click.
    Touchpad,
}

impl Button {
    pub const ALL: [Button; 16] = [
        Self::South,
        Self::East,
        Self::West,
        Self::North,
        Self::Back,
        Self::Guide,
        Self::Start,
        Self::LeftStick,
        Self::RightStick,
        Self::LeftShoulder,
        Self::RightShoulder,
        Self::DpadUp,
        Self::DpadDown,
        Self::DpadLeft,
        Self::DpadRight,
        Self::Touchpad,
    ];

    const fn index(self) -> usize {
        self as usize
    }
}

/// XInput (XUSB) buttons, with their report bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum XButton {
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    Start,
    Back,
    LeftThumb,
    RightThumb,
    LeftShoulder,
    RightShoulder,
    Guide,
    A,
    B,
    X,
    Y,
}

impl XButton {
    pub const fn bit(self) -> u16 {
        match self {
            Self::DpadUp => 0x0001,
            Self::DpadDown => 0x0002,
            Self::DpadLeft => 0x0004,
            Self::DpadRight => 0x0008,
            Self::Start => 0x0010,
            Self::Back => 0x0020,
            Self::LeftThumb => 0x0040,
            Self::RightThumb => 0x0080,
            Self::LeftShoulder => 0x0100,
            Self::RightShoulder => 0x0200,
            Self::Guide => 0x0400,
            Self::A => 0x1000,
            Self::B => 0x2000,
            Self::X => 0x4000,
            Self::Y => 0x8000,
        }
    }
}

/// Current state of one physical pad in the canonical layout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PadState {
    /// Bit `1 << Button::index()` per pressed button.
    pub buttons: u32,
    pub left: (i16, i16),
    pub right: (i16, i16),
    pub left_trigger: i16,
    pub right_trigger: i16,
}

impl PadState {
    pub fn set(&mut self, button: Button, pressed: bool) {
        let bit = 1u32 << button.index();
        if pressed {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
    }
}

/// XUSB report as ViGEmBus and uinput backends consume it. Y points up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct XReport {
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub left: (i16, i16),
    pub right: (i16, i16),
}

/// Per-stick settings, in percent of full deflection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StickSettings {
    /// Radial deadzone (default 8 %).
    pub deadzone: u8,
    /// Output starts here once outside the deadzone (default 0 %).
    pub anti_deadzone: u8,
    pub invert_y: bool,
}

impl Default for StickSettings {
    fn default() -> Self {
        Self {
            deadzone: 8,
            anti_deadzone: 0,
            invert_y: false,
        }
    }
}

/// The user's mapping profile for a package (or the global default), stored
/// as JSON in `controller_profiles.profile`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Profile {
    /// "Use Nintendo button labels": map Switch Pro face buttons by label
    /// instead of by position.
    pub nintendo_labels: bool,
    pub left_stick: StickSettings,
    pub right_stick: StickSettings,
    /// User remaps, applied after the layout (`from` canonical → `to` XInput).
    /// `to: null` disables the button.
    pub remap: Vec<Remap>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Remap {
    pub from: Button,
    pub to: Option<XButton>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    #[error("the profile is not valid JSON for this launcher: {0}")]
    Syntax(String),
    #[error("deadzones must be below 100 % and the anti-deadzone below 100 %")]
    Range,
    #[error("{0:?} is remapped twice")]
    DuplicateRemap(Button),
}

/// Profiles are small; larger input is refused before parsing.
const MAX_PROFILE_BYTES: usize = 16 * 1024;

impl Profile {
    /// Parses and validates stored or imported JSON (untrusted: it may be a
    /// file the player imported).
    pub fn parse(json: &[u8]) -> Result<Self, ProfileError> {
        if json.len() > MAX_PROFILE_BYTES {
            return Err(ProfileError::Syntax("too large".into()));
        }
        let profile: Self =
            serde_json::from_slice(json).map_err(|e| ProfileError::Syntax(e.to_string()))?;
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<(), ProfileError> {
        for stick in [self.left_stick, self.right_stick] {
            if stick.deadzone >= 100 || stick.anti_deadzone >= 100 {
                return Err(ProfileError::Range);
            }
        }
        let mut seen = 0u32;
        for remap in &self.remap {
            let bit = 1u32 << remap.from.index();
            if seen & bit != 0 {
                return Err(ProfileError::DuplicateRemap(remap.from));
            }
            seen |= bit;
        }
        Ok(())
    }
}

/// The layout for one physical pad, precomputed.
#[derive(Debug, Clone)]
pub struct Mapper {
    /// XInput bits per canonical button index.
    table: [u16; Button::ALL.len()],
    left: Stick,
    right: Stick,
}

#[derive(Debug, Clone, Copy)]
struct Stick {
    /// Fractions of full deflection.
    deadzone: f32,
    anti_deadzone: f32,
    invert_y: bool,
}

const AXIS_MAX: f32 = 32767.0;

impl Mapper {
    pub fn new(kind: ControllerKind, profile: &Profile) -> Self {
        let mut table = [0u16; Button::ALL.len()];
        for button in Button::ALL {
            if let Some(slot) = table.get_mut(button.index()) {
                *slot =
                    default_target(kind, profile.nintendo_labels, button).map_or(0, XButton::bit);
            }
        }
        for remap in &profile.remap {
            if let Some(slot) = table.get_mut(remap.from.index()) {
                *slot = remap.to.map_or(0, XButton::bit);
            }
        }
        Self {
            table,
            left: Stick::new(profile.left_stick),
            right: Stick::new(profile.right_stick),
        }
    }

    pub fn map(&self, state: &PadState) -> XReport {
        let mut buttons = 0u16;
        let mut pressed = state.buttons;
        while pressed != 0 {
            let index = pressed.trailing_zeros() as usize;
            buttons |= self.table.get(index).copied().unwrap_or(0);
            pressed &= pressed - 1;
        }
        XReport {
            buttons,
            left_trigger: trigger(state.left_trigger),
            right_trigger: trigger(state.right_trigger),
            left: self.left.map(state.left),
            right: self.right.map(state.right),
        }
    }
}

/// The layout before user remaps: face buttons by position (Cross → A,
/// Circle → B; the Switch Pro's bottom B → A), or by label for Nintendo pads
/// when the player asked for it. Touchpad click → Back, PS/Home → Guide.
fn default_target(kind: ControllerKind, nintendo_labels: bool, button: Button) -> Option<XButton> {
    let by_label = nintendo_labels && kind == ControllerKind::SwitchPro;
    Some(match button {
        // Nintendo labels: A is east, B south, X north, Y west.
        Button::South if by_label => XButton::B,
        Button::East if by_label => XButton::A,
        Button::West if by_label => XButton::Y,
        Button::North if by_label => XButton::X,
        Button::South => XButton::A,
        Button::East => XButton::B,
        Button::West => XButton::X,
        Button::North => XButton::Y,
        Button::Back | Button::Touchpad => XButton::Back,
        Button::Guide => XButton::Guide,
        Button::Start => XButton::Start,
        Button::LeftStick => XButton::LeftThumb,
        Button::RightStick => XButton::RightThumb,
        Button::LeftShoulder => XButton::LeftShoulder,
        Button::RightShoulder => XButton::RightShoulder,
        Button::DpadUp => XButton::DpadUp,
        Button::DpadDown => XButton::DpadDown,
        Button::DpadLeft => XButton::DpadLeft,
        Button::DpadRight => XButton::DpadRight,
    })
}

impl Stick {
    fn new(settings: StickSettings) -> Self {
        Self {
            deadzone: f32::from(settings.deadzone) / 100.0,
            anti_deadzone: f32::from(settings.anti_deadzone) / 100.0,
            invert_y: settings.invert_y,
        }
    }

    /// Radial deadzone with rescaling, then SDL (Y down) → XInput (Y up).
    fn map(&self, (x, y): (i16, i16)) -> (i16, i16) {
        let fx = (f32::from(x) / AXIS_MAX).clamp(-1.0, 1.0);
        let fy = (f32::from(y) / AXIS_MAX).clamp(-1.0, 1.0);
        let magnitude = (fx * fx + fy * fy).sqrt();
        if magnitude <= self.deadzone || magnitude == 0.0 {
            return (0, 0);
        }
        let scaled = ((magnitude - self.deadzone) / (1.0 - self.deadzone)).min(1.0);
        let output = self.anti_deadzone + (1.0 - self.anti_deadzone) * scaled;
        let factor = output / magnitude;
        let up = if self.invert_y { fy } else { -fy };
        (axis(fx * factor), axis(up * factor))
    }
}

#[allow(clippy::cast_possible_truncation)]
fn axis(value: f32) -> i16 {
    // Clamped to ±1 first, so the product fits in i16.
    (value.clamp(-1.0, 1.0) * AXIS_MAX).round() as i16
}

/// SDL 0..=32767 → XInput 0..=255 (negative values are treated as released).
fn trigger(value: i16) -> u8 {
    let value = i32::from(value.max(0));
    u8::try_from((value * 255 + 16383) / 32767).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests;
