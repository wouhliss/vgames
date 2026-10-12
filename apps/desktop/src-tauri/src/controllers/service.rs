//! SDL handles stay on the input owner. IPC reads cached state; held input arms
//! a single cancellable deadline, and releasing it leaves no scheduled task.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use sdl3::GamepadSubsystem;
use sdl3::event::Event;
use sdl3::gamepad::Gamepad;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::input::{InputEvent, InputThread, InputWake};
use super::mapping::Button;
use super::navigation::classify;
use super::routing::{
    Axis, Battery, BatteryState, ConnectedPad, Connection, Output, Router, Tester,
};

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("Controller input is unavailable: {0}")]
    Unavailable(String),
    #[error("Controller state is unavailable.")]
    State,
    #[error(transparent)]
    Tester(#[from] super::routing::TesterError),
}

pub struct Service {
    router: Arc<Mutex<Router>>,
    input: InputThread,
    running: Arc<AtomicBool>,
}
impl Service {
    pub fn start(
        runtime: Handle,
        emit: Arc<dyn Fn(Output) + Send + Sync>,
    ) -> Result<Self, ServiceError> {
        let running = Arc::new(AtomicBool::new(true));
        let stopped = Stopped {
            running: running.clone(),
            emit: emit.clone(),
        };
        let mut routing = Router::new(emit);
        routing.navigation_enabled(false);
        let router = Arc::new(Mutex::new(routing));
        let routed = router.clone();
        let input = InputThread::start_with_wake(move |subsystem, wake| {
            let mut route = sdl_handler(subsystem, routed, runtime, wake);
            move |event: InputEvent, subsystem: &GamepadSubsystem| {
                let _ = &stopped;
                route(event, subsystem);
            }
        })
        .map_err(ServiceError::Unavailable)?;
        Ok(Self {
            router,
            input,
            running,
        })
    }
    fn state(&self) -> Result<MutexGuard<'_, Router>, ServiceError> {
        if !self.running.load(Ordering::Acquire) || !self.input.is_running() {
            return Err(ServiceError::Unavailable(
                "The input worker has stopped.".into(),
            ));
        }
        self.router.lock().map_err(|_| ServiceError::State)
    }
    pub fn list(&self) -> Result<Vec<ConnectedPad>, ServiceError> {
        Ok(self.state()?.list())
    }
    pub fn tester_start(&self, id: u32) -> Result<Tester, ServiceError> {
        let tester = self.state()?.tester_start(id, Instant::now())?;
        self.input.wake().map_err(ServiceError::Unavailable)?;
        Ok(tester)
    }
    pub fn tester_stop(&self, subscription: Uuid) -> Result<(), ServiceError> {
        self.state()?.tester_stop(subscription);
        self.input.wake().map_err(ServiceError::Unavailable)
    }
    pub fn navigation_enabled(&self, enabled: bool) -> Result<(), ServiceError> {
        self.state()?.navigation_enabled(enabled);
        self.input.wake().map_err(ServiceError::Unavailable)
    }
}

struct Stopped {
    running: Arc<AtomicBool>,
    emit: Arc<dyn Fn(Output) + Send + Sync>,
}
impl Drop for Stopped {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (self.emit)(Output::PadsChanged)
        }))
        .is_err()
        {
            tracing::warn!("controller stop notification panicked");
        }
    }
}

fn sdl_handler(
    subsystem: &GamepadSubsystem,
    routed: Arc<Mutex<Router>>,
    runtime: Handle,
    wake: InputWake,
) -> impl FnMut(InputEvent, &GamepadSubsystem) + use<> {
    let mut pads = BTreeMap::<u32, Gamepad>::new();
    let mut deadline = Deadline::new(runtime, wake);
    if let Ok(ids) = subsystem.gamepads() {
        for id in ids {
            open(subsystem, &mut pads, &routed, id);
        }
    }
    move |event: InputEvent, subsystem: &GamepadSubsystem| {
        let now = Instant::now();
        match event {
            InputEvent::Native(Event::GamepadAdded { which, .. }) => {
                open(subsystem, &mut pads, &routed, which);
            }
            InputEvent::Native(Event::GamepadRemoved { which, .. }) => {
                pads.remove(&which.raw());
                if let Ok(mut router) = routed.lock() {
                    router.disconnected(which.raw());
                }
            }
            InputEvent::Native(Event::GamepadButtonDown { which, button, .. }) => {
                if let Some(button) = canonical_button(button)
                    && let Ok(mut router) = routed.lock()
                {
                    router.button(which.raw(), button, true, now);
                }
            }
            InputEvent::Native(Event::GamepadButtonUp { which, button, .. }) => {
                if let Some(button) = canonical_button(button)
                    && let Ok(mut router) = routed.lock()
                {
                    router.button(which.raw(), button, false, now);
                }
            }
            InputEvent::Native(Event::GamepadAxisMotion {
                which, axis, value, ..
            }) => {
                if let Ok(mut router) = routed.lock() {
                    router.axis(which.raw(), canonical_axis(axis), value, now);
                }
            }
            _ => {}
        }
        if let Ok(mut router) = routed.lock() {
            router.tick(now);
            deadline.arm(router.next_deadline());
        }
    }
}

struct Deadline {
    runtime: Handle,
    wake: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    at: Option<Instant>,
    task: Option<JoinHandle<()>>,
}
impl Deadline {
    fn new(runtime: Handle, wake: InputWake) -> Self {
        Self {
            runtime,
            wake: Arc::new(move || wake.wake()),
            at: None,
            task: None,
        }
    }
    fn arm(&mut self, at: Option<Instant>) {
        if self.at == at {
            return;
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.at = at;
        if let Some(at) = at {
            let wake = self.wake.clone();
            self.task = Some(self.runtime.spawn(async move {
                tokio::time::sleep_until(at.into()).await;
                if let Err(error) = wake() {
                    tracing::warn!(%error, "cannot wake held controller input");
                }
            }));
        }
    }
}
impl Drop for Deadline {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn open(
    subsystem: &GamepadSubsystem,
    pads: &mut BTreeMap<u32, Gamepad>,
    router: &Mutex<Router>,
    id: sdl3::joystick::JoystickId,
) {
    if pads.contains_key(&id.raw()) {
        return;
    }
    match subsystem.open(id) {
        Ok(pad) => {
            let power = pad.power_info();
            let info = ConnectedPad {
                instance_id: id.raw(),
                kind: classify(pad.real_type()),
                name: pad.name().unwrap_or_else(|| "Controller".into()),
                // SDL reports wireless, not its transport; a proprietary radio
                // must not be presented as Bluetooth.
                connection: match pad.connection_state() {
                    Ok(sdl3::joystick::ConnectionState::Wired) => Connection::Usb,
                    _ => Connection::Unknown,
                },
                battery: battery(power.state, power.percentage),
                player_index: pad.player_index(),
            };
            pads.insert(id.raw(), pad);
            if let Ok(mut router) = router.lock() {
                router.connected(info);
            }
        }
        Err(error) => tracing::warn!(%id, %error, "cannot open connected controller"),
    }
}
fn battery(power: sdl3::joystick::PowerLevel, percentage: i32) -> Battery {
    use sdl3::joystick::PowerLevel;
    let state = match power {
        PowerLevel::Charging | PowerLevel::Charged => BatteryState::Charging,
        PowerLevel::OnBattery => BatteryState::OnBattery,
        PowerLevel::NoBattery => BatteryState::Wired,
        _ => BatteryState::Unknown,
    };
    Battery {
        percent: ((0..=100).contains(&percentage)
            && matches!(
                power,
                PowerLevel::Charging | PowerLevel::Charged | PowerLevel::OnBattery
            ))
        .then(|| u8::try_from(percentage).ok())
        .flatten(),
        state,
    }
}
fn canonical_axis(axis: sdl3::gamepad::Axis) -> Axis {
    use sdl3::gamepad::Axis as Sdl;
    match axis {
        Sdl::LeftX => Axis::LeftX,
        Sdl::LeftY => Axis::LeftY,
        Sdl::RightX => Axis::RightX,
        Sdl::RightY => Axis::RightY,
        Sdl::TriggerLeft => Axis::LeftTrigger,
        Sdl::TriggerRight => Axis::RightTrigger,
    }
}
fn canonical_button(button: sdl3::gamepad::Button) -> Option<Button> {
    use sdl3::gamepad::Button as Sdl;
    Some(match button {
        Sdl::South => Button::South,
        Sdl::East => Button::East,
        Sdl::West => Button::West,
        Sdl::North => Button::North,
        Sdl::Back => Button::Back,
        Sdl::Guide => Button::Guide,
        Sdl::Start => Button::Start,
        Sdl::LeftStick => Button::LeftStick,
        Sdl::RightStick => Button::RightStick,
        Sdl::LeftShoulder => Button::LeftShoulder,
        Sdl::RightShoulder => Button::RightShoulder,
        Sdl::DPadUp => Button::DpadUp,
        Sdl::DPadDown => Button::DpadDown,
        Sdl::DPadLeft => Button::DpadLeft,
        Sdl::DPadRight => Button::DpadRight,
        Sdl::Touchpad => Button::Touchpad,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_stop_marks_input_unavailable_before_notifying_the_screen() {
        let running = Arc::new(AtomicBool::new(true));
        let observed = running.clone();
        let notifications = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = notifications.clone();
        let stopped = Stopped {
            running: running.clone(),
            emit: Arc::new(move |output| {
                assert!(!observed.load(Ordering::Acquire));
                assert!(matches!(output, Output::PadsChanged));
                count.fetch_add(1, Ordering::Relaxed);
            }),
        };
        drop(stopped);
        assert!(!running.load(Ordering::Acquire));
        assert_eq!(notifications.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn native_sdl_hotplug_routes_buttons_testers_and_disconnect() {
        use sdl3::joystick::{JoystickType, VirtualJoystickDescription};
        use std::sync::mpsc;
        use std::time::Duration;
        // A panic in the other SDL test poisons the lock; report that test's
        // failure once instead of failing here too.
        let _serial = super::super::input::SDL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (outputs, receive) = mpsc::channel();
        let router = Arc::new(Mutex::new(Router::new(Arc::new(move |output| {
            outputs.send(output).unwrap();
        }))));
        let routed = router.clone();
        enum Control {
            Plug,
            Button(bool),
            Unplug,
        }
        let (send, controls) = mpsc::channel();
        let (ready, wake) = mpsc::channel();
        let handle = runtime.handle().clone();
        let mut input = InputThread::start_with_wake(move |subsystem, wake| {
            let mut route = sdl_handler(subsystem, routed, handle, wake.clone());
            let sdl = sdl3::init().unwrap();
            let joystick = sdl.joystick().unwrap();
            let mut connection = None;
            ready.send(wake).unwrap();
            move |event: InputEvent, subsystem: &GamepadSubsystem| {
                if matches!(event, InputEvent::Wake) {
                    while let Ok(control) = controls.try_recv() {
                        match control {
                            Control::Plug => {
                                connection = Some(
                                    joystick
                                        .attach_virtual_joystick(
                                            VirtualJoystickDescription::new()
                                                .name("vgames software fixture")
                                                .joystick_type(JoystickType::Gamepad)
                                                .with_button(sdl3::gamepad::Button::South)
                                                .with_axis(sdl3::gamepad::Axis::LeftX),
                                        )
                                        .unwrap(),
                                )
                            }
                            Control::Button(down) => joystick
                                .open(connection.as_ref().unwrap().id())
                                .unwrap()
                                .set_virtual_button(sdl3::gamepad::Button::South as u32, down)
                                .unwrap(),
                            Control::Unplug => {
                                connection = None;
                            }
                        }
                    }
                }
                route(event, subsystem);
            }
        })
        .unwrap();
        let wake = wake.recv_timeout(Duration::from_secs(1)).unwrap();
        let next = |accept: &dyn Fn(&Output) -> bool| {
            let until = Instant::now() + Duration::from_secs(3);
            loop {
                let output = receive
                    .recv_timeout(until.saturating_duration_since(Instant::now()))
                    .unwrap();
                if accept(&output) {
                    return output;
                }
            }
        };
        send.send(Control::Plug).unwrap();
        wake.wake().unwrap();
        let Output::Connected(pad) = next(&|output| matches!(output, Output::Connected(_))) else {
            unreachable!()
        };
        assert_eq!(pad.name, "vgames software fixture");
        let subscription = router
            .lock()
            .unwrap()
            .tester_start(pad.instance_id, Instant::now())
            .unwrap();
        send.send(Control::Button(true)).unwrap();
        wake.wake().unwrap();
        let Output::Navigation { action, repeat, .. } =
            next(&|output| matches!(output, Output::Navigation { .. }))
        else {
            unreachable!()
        };
        assert_eq!(action, super::super::navigation::NavAction::Accept);
        assert!(!repeat);
        let Output::Tester(state) = next(
            &|output| matches!(output, Output::Tester(state) if state.buttons.contains(&Button::South)),
        ) else {
            unreachable!()
        };
        assert_eq!(state.subscription_id, subscription.subscription_id);
        send.send(Control::Button(false)).unwrap();
        send.send(Control::Unplug).unwrap();
        wake.wake().unwrap();
        let Output::Disconnected(id) = next(&|output| matches!(output, Output::Disconnected(_)))
        else {
            unreachable!()
        };
        assert_eq!(id, pad.instance_id);
        assert!(router.lock().unwrap().list().is_empty());
        assert!(router.lock().unwrap().next_deadline().is_none());
        input.stop();
    }

    #[tokio::test]
    async fn releasing_the_last_input_cancels_the_only_scheduled_wakeup() {
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        let mut deadline = Deadline {
            runtime: Handle::current(),
            wake: Arc::new(move || send.send(()).map_err(|error| error.to_string())),
            at: None,
            task: None,
        };
        deadline.arm(Some(Instant::now() + std::time::Duration::from_millis(25)));
        deadline.arm(None);
        assert!(deadline.task.is_none());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(80), receive.recv())
                .await
                .is_err()
        );
        deadline.arm(Some(Instant::now()));
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), receive.recv())
                .await
                .unwrap(),
            Some(())
        );
        deadline.arm(None);
        assert!(deadline.task.is_none());
    }

    #[test]
    fn unknown_and_invalid_battery_values_are_not_invented() {
        use sdl3::joystick::PowerLevel;
        assert_eq!(battery(PowerLevel::Unknown, 0).percent, None);
        assert_eq!(battery(PowerLevel::NoBattery, 0).percent, None);
        assert_eq!(battery(PowerLevel::OnBattery, -1).percent, None);
        assert_eq!(battery(PowerLevel::OnBattery, 101).percent, None);
        assert_eq!(battery(PowerLevel::Charging, 100).percent, Some(100));
        assert_eq!(canonical_button(sdl3::gamepad::Button::Misc1), None);
        assert_eq!(
            canonical_button(sdl3::gamepad::Button::Touchpad),
            Some(Button::Touchpad)
        );
    }
}
