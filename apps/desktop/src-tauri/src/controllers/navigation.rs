//! Controller navigation without polling. The input service arms a wakeup only
//! for the next held-direction repeat or Guide deadline; idle has no deadline.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use specta::Type;

use super::mapping::Button;
use crate::events::ControllerKind;

const FIRST_REPEAT: Duration = Duration::from_millis(350);
const NEXT_REPEAT: Duration = Duration::from_millis(100);
const GUIDE_HOLD: Duration = Duration::from_secs(1);
const STICK_PRESS: i32 = 18_022;
const STICK_RELEASE: i32 = 13_107;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum NavAction {
    Up,
    Down,
    Left,
    Right,
    Accept,
    Back,
    Menu,
    Options,
    TabPrev,
    TabNext,
}

impl NavAction {
    const ALL: [Self; 10] = [
        Self::Up,
        Self::Down,
        Self::Left,
        Self::Right,
        Self::Accept,
        Self::Back,
        Self::Menu,
        Self::Options,
        Self::TabPrev,
        Self::TabNext,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intent {
    pub action: NavAction,
    pub repeat: bool,
}

/// Fixed-size output keeps navigation input allocation-free.
#[derive(Debug, Default)]
pub struct Frame {
    pub intents: [Option<Intent>; 10],
    pub guide_held: bool,
    pub meaningful_input: bool,
}

#[derive(Default)]
pub struct Navigation {
    buttons: u32,
    stick: [bool; 4],
    held: [bool; 10],
    repeats: [Option<Instant>; 4],
    guide: Option<Instant>,
}

impl Navigation {
    pub fn button(&mut self, button: Button, down: bool, now: Instant) -> Frame {
        let bit = 1u32 << button as u32;
        let changed = (self.buttons & bit != 0) != down;
        if down {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
        if button == Button::Guide && changed {
            self.guide = down.then_some(now + GUIDE_HOLD);
        }
        let mut frame = self.reconcile(now);
        frame.meaningful_input = changed && down;
        frame
    }

    pub fn left_stick(&mut self, x: i16, y: i16, now: Instant) -> Frame {
        let previous = self.stick;
        for ((held, old), deflection) in self.stick.iter_mut().zip(previous).zip([
            -i32::from(y),
            i32::from(y),
            -i32::from(x),
            i32::from(x),
        ]) {
            *held = deflection >= if old { STICK_RELEASE } else { STICK_PRESS };
        }
        let mut frame = self.reconcile(now);
        frame.meaningful_input = self
            .stick
            .iter()
            .zip(previous)
            .any(|(&held, old)| held && !old);
        frame
    }

    fn pressed(&self, button: Button) -> bool {
        self.buttons & (1u32 << button as u32) != 0
    }

    fn reconcile(&mut self, now: Instant) -> Frame {
        let [up, down, left, right] = self.stick;
        let current = [
            self.pressed(Button::DpadUp) || up,
            self.pressed(Button::DpadDown) || down,
            self.pressed(Button::DpadLeft) || left,
            self.pressed(Button::DpadRight) || right,
            self.pressed(Button::South),
            self.pressed(Button::East) || self.pressed(Button::Back),
            self.pressed(Button::North),
            self.pressed(Button::Start),
            self.pressed(Button::LeftShoulder),
            self.pressed(Button::RightShoulder),
        ];
        let mut frame = Frame::default();
        for (((held, old), intent), action) in current
            .into_iter()
            .zip(self.held)
            .zip(&mut frame.intents)
            .zip(NavAction::ALL)
        {
            if held && !old {
                *intent = Some(Intent {
                    action,
                    repeat: false,
                });
            }
        }
        for ((deadline, held), old) in self.repeats.iter_mut().zip(current).zip(self.held) {
            if held && !old {
                *deadline = Some(now + FIRST_REPEAT);
            } else if !held {
                *deadline = None;
            }
        }
        self.held = current;
        frame
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.repeats
            .iter()
            .copied()
            .flatten()
            .chain(self.guide)
            .min()
    }

    pub fn tick(&mut self, now: Instant) -> Frame {
        let mut frame = Frame::default();
        for ((deadline, intent), action) in self
            .repeats
            .iter_mut()
            .zip(&mut frame.intents)
            .zip(NavAction::ALL)
        {
            if deadline.is_some_and(|deadline| deadline <= now) {
                *intent = Some(Intent {
                    action,
                    repeat: true,
                });
                // A delayed event never floods the UI with a catch-up burst.
                *deadline = Some(now + NEXT_REPEAT);
            }
        }
        if self.guide.is_some_and(|deadline| deadline <= now) {
            self.guide = None;
            frame.guide_held = true;
        }
        frame
    }

    /// Hot-unplug cancels every held input and armed deadline.
    pub fn disconnect(&mut self) {
        *self = Self::default();
    }
}

pub fn classify(kind: sdl3::gamepad::GamepadType) -> ControllerKind {
    use sdl3::gamepad::GamepadType;
    match kind {
        GamepadType::Xbox360 | GamepadType::XboxOne => ControllerKind::Xinput,
        GamepadType::PS4 => ControllerKind::Dualshock4,
        GamepadType::PS5 => ControllerKind::Dualsense,
        GamepadType::NintendoSwitchPro => ControllerKind::SwitchPro,
        _ => ControllerKind::Generic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intents(frame: Frame) -> Vec<Intent> {
        frame.intents.into_iter().flatten().collect()
    }

    #[test]
    fn presses_debounce_and_accept_never_repeats() {
        let now = Instant::now();
        let mut nav = Navigation::default();
        assert!(nav.next_deadline().is_none());
        assert_eq!(
            intents(nav.button(Button::South, true, now)),
            [Intent {
                action: NavAction::Accept,
                repeat: false
            }]
        );
        assert!(intents(nav.button(Button::South, true, now)).is_empty());
        assert!(nav.next_deadline().is_none());
        assert!(intents(nav.tick(now + Duration::from_secs(20))).is_empty());
        nav.button(Button::South, false, now);
        assert_eq!(intents(nav.button(Button::South, true, now)).len(), 1);
    }

    #[test]
    fn directions_repeat_only_while_held_without_catchup_bursts() {
        let now = Instant::now();
        let mut nav = Navigation::default();
        assert_eq!(intents(nav.button(Button::DpadRight, true, now)).len(), 1);
        assert_eq!(nav.next_deadline(), Some(now + FIRST_REPEAT));
        assert!(intents(nav.tick(now + FIRST_REPEAT - Duration::from_millis(1))).is_empty());
        assert_eq!(
            intents(nav.tick(now + FIRST_REPEAT)),
            [Intent {
                action: NavAction::Right,
                repeat: true
            }]
        );
        assert_eq!(intents(nav.tick(now + Duration::from_secs(10))).len(), 1);
        nav.button(Button::DpadRight, false, now);
        assert!(nav.next_deadline().is_none());
        assert!(intents(nav.tick(now + Duration::from_secs(30))).is_empty());
    }

    #[test]
    fn stick_hysteresis_filters_jitter_and_handles_minimum_axis() {
        let now = Instant::now();
        let mut nav = Navigation::default();
        assert!(!nav.left_stick(0, 100, now).meaningful_input);
        assert!(nav.next_deadline().is_none());
        assert_eq!(intents(nav.left_stick(20_000, 0, now)).len(), 1);
        assert!(intents(nav.left_stick(15_000, 0, now)).is_empty());
        assert!(intents(nav.left_stick(17_000, 0, now)).is_empty());
        nav.left_stick(12_000, 0, now);
        assert!(nav.next_deadline().is_none());
        assert_eq!(
            intents(nav.left_stick(i16::MIN, 0, now)),
            [Intent {
                action: NavAction::Left,
                repeat: false
            }]
        );
    }

    #[test]
    fn stick_and_dpad_share_debounce_and_disconnect_resets_everything() {
        let now = Instant::now();
        let mut nav = Navigation::default();
        nav.left_stick(20_000, 0, now);
        assert!(intents(nav.button(Button::DpadRight, true, now)).is_empty());
        nav.left_stick(0, 0, now);
        assert!(nav.next_deadline().is_some());
        nav.disconnect();
        assert!(nav.next_deadline().is_none());
        assert!(intents(nav.tick(now + Duration::from_secs(5))).is_empty());
        assert_eq!(intents(nav.button(Button::DpadRight, true, now)).len(), 1);
    }

    #[test]
    fn guide_requires_one_second_and_fires_once_per_hold() {
        let now = Instant::now();
        let mut nav = Navigation::default();
        nav.button(Button::Guide, true, now);
        assert!(!nav.tick(now + Duration::from_millis(999)).guide_held);
        assert!(nav.tick(now + GUIDE_HOLD).guide_held);
        assert!(nav.next_deadline().is_none());
        assert!(!nav.tick(now + Duration::from_secs(5)).guide_held);
        nav.button(Button::Guide, false, now);
        nav.button(Button::Guide, true, now);
        nav.disconnect();
        assert!(!nav.tick(now + GUIDE_HOLD).guide_held);
    }

    #[test]
    fn sdl_classification_keeps_unknown_pads_generic() {
        use sdl3::gamepad::GamepadType as G;
        for (input, expected) in [
            (G::Xbox360, ControllerKind::Xinput),
            (G::XboxOne, ControllerKind::Xinput),
            (G::PS4, ControllerKind::Dualshock4),
            (G::PS5, ControllerKind::Dualsense),
            (G::NintendoSwitchPro, ControllerKind::SwitchPro),
            (G::Unknown, ControllerKind::Generic),
            (G::NintendoSwitchJoyconPair, ControllerKind::Generic),
        ] {
            assert_eq!(classify(input), expected);
        }
    }
}
