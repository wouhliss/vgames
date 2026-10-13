//! Event-driven controller routing. Metadata and canonical state are cached;
//! callers arm exactly `next_deadline()` and cancel it when that becomes None.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

use super::mapping::{Button, PadState};
use super::navigation::{Frame, NavAction, Navigation};
use crate::events::ControllerKind;

const TESTER_PERIOD: Duration = Duration::from_nanos(33_333_334);
const MAX_TESTERS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Connection {
    Usb,
    Bluetooth,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum BatteryState {
    Charging,
    OnBattery,
    Wired,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct Battery {
    pub percent: Option<u8>,
    pub state: BatteryState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ConnectedPad {
    pub instance_id: u32,
    pub kind: ControllerKind,
    pub name: String,
    pub connection: Connection,
    pub battery: Battery,
    pub player_index: Option<u16>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type)]
pub struct Tester {
    pub subscription_id: Uuid,
    pub instance_id: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct TesterState {
    pub subscription_id: Uuid,
    pub instance_id: u32,
    pub buttons: Vec<Button>,
    pub left: (i16, i16),
    pub right: (i16, i16),
    pub left_trigger: i16,
    pub right_trigger: i16,
}
#[derive(Debug, Clone)]
pub enum Output {
    Navigation {
        action: NavAction,
        controller: ControllerKind,
        repeat: bool,
    },
    ActiveControllerChanged(Option<ControllerKind>),
    Connected(ConnectedPad),
    Disconnected(u32),
    PadsChanged,
    GuideHeld(u32),
    Tester(TesterState),
}
#[derive(Debug, Clone, Copy)]
pub enum Axis {
    LeftX,
    LeftY,
    RightX,
    RightY,
    LeftTrigger,
    RightTrigger,
}
#[derive(Debug, thiserror::Error)]
pub enum TesterError {
    #[error("The controller is no longer connected.")]
    NotConnected,
    #[error("Close another controller tester before opening this one.")]
    TooMany,
}
struct RoutedPad {
    info: ConnectedPad,
    state: PadState,
    nav: Navigation,
    tester_dirty: bool,
    tester_sent: Option<Instant>,
}

pub struct Router {
    pads: BTreeMap<u32, RoutedPad>,
    testers: BTreeMap<Uuid, u32>,
    active: Option<u32>,
    navigation_enabled: bool,
    emit: Arc<dyn Fn(Output) + Send + Sync>,
}
impl Router {
    pub fn new(emit: Arc<dyn Fn(Output) + Send + Sync>) -> Self {
        Self {
            pads: BTreeMap::new(),
            testers: BTreeMap::new(),
            active: None,
            navigation_enabled: true,
            emit,
        }
    }
    pub fn list(&self) -> Vec<ConnectedPad> {
        let mut pads: Vec<_> = self.pads.values().map(|pad| pad.info.clone()).collect();
        pads.sort_by_key(|pad| (pad.player_index, pad.instance_id));
        pads
    }
    pub fn physical_pads(&self) -> Vec<super::sessions::PhysicalPad> {
        self.pads
            .values()
            .map(|pad| super::sessions::PhysicalPad {
                instance_id: pad.info.instance_id,
                kind: pad.info.kind,
                player_index: pad.info.player_index,
                state: pad.state,
            })
            .collect()
    }
    pub fn pad_state(&self, id: u32) -> Option<PadState> {
        self.pads.get(&id).map(|pad| pad.state)
    }
    pub fn navigation_enabled(&mut self, enabled: bool) {
        self.navigation_enabled = enabled;
    }
    pub fn connected(&mut self, info: ConnectedPad) {
        if self.pads.contains_key(&info.instance_id) {
            return;
        }
        self.pads.insert(
            info.instance_id,
            RoutedPad {
                info: info.clone(),
                state: PadState::default(),
                nav: Navigation::default(),
                tester_dirty: false,
                tester_sent: None,
            },
        );
        (self.emit)(Output::Connected(info));
        (self.emit)(Output::PadsChanged);
    }
    pub fn disconnected(&mut self, id: u32) {
        if self.pads.remove(&id).is_none() {
            return;
        }
        self.testers.retain(|_, pad| *pad != id);
        if self.active == Some(id) {
            self.active = None;
            (self.emit)(Output::ActiveControllerChanged(None));
        }
        (self.emit)(Output::Disconnected(id));
        (self.emit)(Output::PadsChanged);
    }
    pub fn button(&mut self, id: u32, button: Button, down: bool, now: Instant) {
        let Some(pad) = self.pads.get_mut(&id) else {
            return;
        };
        let old = pad.state;
        pad.state.set(button, down);
        let frame = pad.nav.button(button, down, now);
        let changed = old != pad.state;
        self.frame(id, frame);
        if changed {
            self.tester_changed(id, now);
        }
    }
    pub fn axis(&mut self, id: u32, axis: Axis, value: i16, now: Instant) {
        let Some(pad) = self.pads.get_mut(&id) else {
            return;
        };
        let old = pad.state;
        match axis {
            Axis::LeftX => pad.state.left.0 = value,
            Axis::LeftY => pad.state.left.1 = value,
            Axis::RightX => pad.state.right.0 = value,
            Axis::RightY => pad.state.right.1 = value,
            Axis::LeftTrigger => pad.state.left_trigger = value.max(0),
            Axis::RightTrigger => pad.state.right_trigger = value.max(0),
        }
        let changed = old != pad.state;
        let mut frame = match axis {
            Axis::LeftX | Axis::LeftY => {
                pad.nav.left_stick(pad.state.left.0, pad.state.left.1, now)
            }
            _ => Frame::default(),
        };
        frame.meaningful_input |= changed
            && match axis {
                Axis::LeftTrigger | Axis::RightTrigger => value >= 2_622,
                _ => i32::from(value).abs() >= 2_622,
            };
        self.frame(id, frame);
        if changed {
            self.tester_changed(id, now);
        }
    }
    fn frame(&mut self, id: u32, frame: Frame) {
        let Some(pad) = self.pads.get(&id) else {
            return;
        };
        let kind = pad.info.kind;
        if frame.meaningful_input && self.active != Some(id) {
            let previous = self
                .active
                .and_then(|active| self.pads.get(&active))
                .map(|pad| pad.info.kind);
            self.active = Some(id);
            if previous != Some(kind) {
                (self.emit)(Output::ActiveControllerChanged(Some(kind)));
            }
        }
        if self.navigation_enabled {
            for intent in frame.intents.into_iter().flatten() {
                (self.emit)(Output::Navigation {
                    action: intent.action,
                    controller: kind,
                    repeat: intent.repeat,
                });
            }
        }
        if frame.guide_held {
            (self.emit)(Output::GuideHeld(id));
        }
    }
    pub fn tick(&mut self, now: Instant) {
        // Frames contain no meaningful input on ticks, so active identity cannot
        // change. Emit directly while borrowing the pad map, without allocating.
        for (&id, pad) in &mut self.pads {
            let frame = pad.nav.tick(now);
            if self.navigation_enabled {
                for intent in frame.intents.into_iter().flatten() {
                    (self.emit)(Output::Navigation {
                        action: intent.action,
                        controller: pad.info.kind,
                        repeat: intent.repeat,
                    });
                }
            }
            if frame.guide_held {
                (self.emit)(Output::GuideHeld(id));
            }
            if pad.tester_dirty
                && pad
                    .tester_sent
                    .is_none_or(|at| now.duration_since(at) >= TESTER_PERIOD)
            {
                for (&subscription_id, &instance_id) in &self.testers {
                    if instance_id == id {
                        (self.emit)(Output::Tester(snapshot(subscription_id, id, pad.state)));
                    }
                }
                pad.tester_dirty = false;
                pad.tester_sent = Some(now);
            }
        }
    }
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pads
            .values()
            .flat_map(|pad| {
                [
                    pad.nav.next_deadline(),
                    if pad.tester_dirty {
                        pad.tester_sent.map(|at| at + TESTER_PERIOD)
                    } else {
                        None
                    },
                ]
            })
            .flatten()
            .min()
    }
    pub fn tester_start(&mut self, id: u32, now: Instant) -> Result<Tester, TesterError> {
        let pad = self.pads.get_mut(&id).ok_or(TesterError::NotConnected)?;
        if self.testers.len() >= MAX_TESTERS {
            return Err(TesterError::TooMany);
        }
        let subscription_id = Uuid::new_v4();
        self.testers.insert(subscription_id, id);
        pad.tester_sent = Some(now);
        (self.emit)(Output::Tester(snapshot(subscription_id, id, pad.state)));
        Ok(Tester {
            subscription_id,
            instance_id: id,
        })
    }
    pub fn tester_stop(&mut self, subscription_id: Uuid) {
        if let Some(id) = self.testers.remove(&subscription_id)
            && !self.testers.values().any(|pad| *pad == id)
            && let Some(pad) = self.pads.get_mut(&id)
        {
            pad.tester_dirty = false;
            pad.tester_sent = None;
        }
    }
    fn tester_changed(&mut self, id: u32, now: Instant) {
        if !self.testers.values().any(|pad| *pad == id) {
            return;
        }
        if let Some(pad) = self.pads.get_mut(&id) {
            pad.tester_dirty = true;
        }
        self.tick(now);
    }
}
fn snapshot(subscription_id: Uuid, instance_id: u32, state: PadState) -> TesterState {
    TesterState {
        subscription_id,
        instance_id,
        buttons: Button::ALL
            .into_iter()
            .filter(|button| state.buttons & (1 << *button as u32) != 0)
            .collect(),
        left: state.left,
        right: state.right,
        left_trigger: state.left_trigger,
        right_trigger: state.right_trigger,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    fn setup() -> (Router, Arc<Mutex<Vec<Output>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        (
            Router::new(Arc::new(move |event| sink.lock().unwrap().push(event))),
            events,
        )
    }
    fn pad(id: u32, kind: ControllerKind) -> ConnectedPad {
        ConnectedPad {
            instance_id: id,
            kind,
            name: "Pad".into(),
            connection: Connection::Unknown,
            battery: Battery {
                percent: None,
                state: BatteryState::Unknown,
            },
            player_index: None,
        }
    }
    #[test]
    fn input_debounces_changes_active_kind_and_disconnect_cancels_all_deadlines() {
        let (mut router, events) = setup();
        let now = Instant::now();
        router.connected(pad(1, ControllerKind::Dualsense));
        router.axis(1, Axis::LeftX, 5, now);
        assert!(
            !events
                .lock()
                .unwrap()
                .iter()
                .any(|event| matches!(event, Output::ActiveControllerChanged(_)))
        );
        router.button(1, Button::South, true, now);
        router.button(1, Button::South, true, now);
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| matches!(
                    event,
                    Output::Navigation {
                        action: NavAction::Accept,
                        ..
                    }
                ))
                .count(),
            1
        );
        router.button(1, Button::Guide, true, now);
        router.button(1, Button::DpadUp, true, now);
        router.tick(now + Duration::from_secs(1));
        router.tick(now + Duration::from_secs(2));
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| matches!(event, Output::GuideHeld(1)))
                .count(),
            1
        );
        router.disconnected(1);
        assert!(router.next_deadline().is_none());
        assert!(
            events
                .lock()
                .unwrap()
                .iter()
                .any(|event| matches!(event, Output::ActiveControllerChanged(None)))
        );
    }
    #[test]
    fn tester_is_scoped_throttled_and_has_no_timer_after_stop() {
        let (mut router, events) = setup();
        let now = Instant::now();
        router.connected(pad(1, ControllerKind::Xinput));
        router.connected(pad(2, ControllerKind::Dualsense));
        let tester = router.tester_start(1, now).unwrap();
        events.lock().unwrap().clear();
        for value in 1..30 {
            router.axis(1, Axis::RightX, value, now + Duration::from_millis(1));
        }
        router.axis(2, Axis::RightX, 30_000, now + Duration::from_millis(1));
        assert!(
            !events
                .lock()
                .unwrap()
                .iter()
                .any(|event| matches!(event, Output::Tester(_)))
        );
        router.tick(now + TESTER_PERIOD);
        let emitted = events.lock().unwrap();
        let streams: Vec<_> = emitted
            .iter()
            .filter_map(|event| {
                if let Output::Tester(value) = event {
                    Some(value)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].subscription_id, tester.subscription_id);
        assert_eq!(streams[0].right, (29, 0));
        drop(emitted);
        router.tester_stop(tester.subscription_id);
        router.tester_stop(tester.subscription_id);
        assert!(router.next_deadline().is_none());
        router.axis(1, Axis::RightX, 99, now + Duration::from_secs(1));
        assert!(router.next_deadline().is_none());
    }
    #[test]
    fn disconnect_removes_subscriptions_and_preserves_the_other_pad() {
        let (mut router, events) = setup();
        let now = Instant::now();
        router.connected(pad(1, ControllerKind::Dualsense));
        router.connected(pad(2, ControllerKind::Xinput));
        router.tester_start(1, now).unwrap();
        router.disconnected(1);
        events.lock().unwrap().clear();
        assert!(matches!(
            router.tester_start(1, now),
            Err(TesterError::NotConnected)
        ));
        router.button(2, Button::South, true, now);
        assert!(events.lock().unwrap().iter().any(|event| matches!(
            event,
            Output::Navigation {
                controller: ControllerKind::Xinput,
                ..
            }
        )));
        assert!(
            !events
                .lock()
                .unwrap()
                .iter()
                .any(|event| matches!(event, Output::Tester(_)))
        );
        assert_eq!(router.list().len(), 1);
    }
}
