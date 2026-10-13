//! Required hosted Linux kernel fixture; the desktop matrix explicitly runs
//! this ignored test as root after compiling it as the ordinary runner user.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, Device, EventType, InputEvent, InputId,
    KeyCode, UinputAbsSetup,
};
use sdl3::event::Event;
use vgames_desktop_lib::controllers::backends::linux::{DEVICE_NAME, Linux};
use vgames_desktop_lib::controllers::backends::{
    EmulatedController, VirtualPad, VirtualPadBackend,
};
use vgames_desktop_lib::controllers::input::{InputEvent as SdlInput, InputThread};
use vgames_desktop_lib::controllers::mapping::{Button, Mapper, PadState, Profile};
use vgames_desktop_lib::events::ControllerKind;

const PREFIX: &str = "vgames kernel physical ";
#[allow(clippy::unwrap_used)]
fn physical(index: usize) -> VirtualDevice {
    let name = format!("{PREFIX}{index}");
    let keys: AttributeSet<KeyCode> = [
        KeyCode::BTN_SOUTH,
        KeyCode::BTN_EAST,
        KeyCode::BTN_NORTH,
        KeyCode::BTN_WEST,
        KeyCode::BTN_TL,
        KeyCode::BTN_TR,
        KeyCode::BTN_SELECT,
        KeyCode::BTN_START,
        KeyCode::BTN_MODE,
        KeyCode::BTN_THUMBL,
        KeyCode::BTN_THUMBR,
    ]
    .into_iter()
    .collect();
    let mut builder = VirtualDevice::builder()
        .unwrap()
        .name(&name)
        .input_id(InputId::new(BusType::BUS_USB, 0x054c, 0x0ce6, 0x0100))
        .with_keys(&keys)
        .unwrap();
    for axis in [
        AbsoluteAxisCode::ABS_X,
        AbsoluteAxisCode::ABS_Y,
        AbsoluteAxisCode::ABS_RX,
        AbsoluteAxisCode::ABS_RY,
    ] {
        builder = builder
            .with_absolute_axis(&UinputAbsSetup::new(
                axis,
                AbsInfo::new(0, -32768, 32767, 0, 0, 0),
            ))
            .unwrap();
    }
    for axis in [AbsoluteAxisCode::ABS_Z, AbsoluteAxisCode::ABS_RZ] {
        builder = builder
            .with_absolute_axis(&UinputAbsSetup::new(axis, AbsInfo::new(0, 0, 255, 0, 0, 0)))
            .unwrap();
    }
    builder.build().unwrap()
}
#[allow(clippy::unwrap_used)]
fn receive_button(device: &mut Device, down: bool) {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let left = until.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "virtual report did not arrive");
        let mut ready = libc::pollfd {
            fd: device.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        #[allow(unsafe_code)]
        // SAFETY: poll borrows one live evdev descriptor and writes this one-element array.
        let result = unsafe {
            libc::poll(
                &mut ready,
                1,
                i32::try_from(left.as_millis()).unwrap_or(i32::MAX).max(1),
            )
        };
        if result < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        assert!(result > 0, "virtual report timed out");
        for event in device.fetch_events().unwrap() {
            if event.event_type() == EventType::KEY
                && event.code() == KeyCode::BTN_SOUTH.0
                && event.value() == i32::from(down)
            {
                return;
            }
        }
    }
}
#[test]
#[ignore = "Requires modprobe uinput and root; required desktop-matrix Linux step runs it."]
fn kernel_hotplug_reports_and_rumble_are_event_driven_with_p99_under_two_ms() {
    let (rumble, feedback) = mpsc::channel();
    let mut outputs = Vec::<Option<Box<dyn VirtualPad>>>::new();
    let mut observers = Vec::new();
    for index in 0..2 {
        let rumble = rumble.clone();
        let mut pad = Linux
            .create(
                EmulatedController::Xbox360,
                index,
                Arc::new(move |value| {
                    rumble.send(value).unwrap();
                }),
            )
            .unwrap();
        let node = pad
            .node_path()
            .unwrap()
            .expect("uinput created an evdev node");
        let device = Device::open(node).unwrap();
        device.set_nonblocking(true).unwrap();
        observers.push(device);
        outputs.push(Some(pad));
    }
    let (connected, connection) = mpsc::channel();
    let (disconnected, disconnection) = mpsc::channel();
    let mut input = InputThread::start(move |_| {
        let mut pads = BTreeMap::new();
        let joysticks = sdl3::init().unwrap().joystick().unwrap();
        let mapper = Mapper::new(ControllerKind::Dualsense, &Profile::default());
        move |event: SdlInput, subsystem: &sdl3::GamepadSubsystem| match event {
            SdlInput::Native(Event::GamepadAdded { which, .. }) => {
                let pad = subsystem.open(which).unwrap();
                // Use the kernel name: mapping databases may replace the gamepad name.
                let name = joysticks.open(which).unwrap().name();
                if name == DEVICE_NAME {
                    return;
                }
                if let Some(index) = name
                    .strip_prefix(PREFIX)
                    .and_then(|index| index.parse::<usize>().ok())
                    && let Some(output) = outputs.get_mut(index).and_then(Option::take)
                {
                    pads.insert(which, (pad, PadState::default(), output, index));
                    connected.send(index).unwrap();
                }
            }
            SdlInput::Native(Event::GamepadButtonDown {
                which,
                button: sdl3::gamepad::Button::South,
                ..
            }) => {
                if let Some((_, state, output, _)) = pads.get_mut(&which) {
                    state.set(Button::South, true);
                    output.update(&mapper.map(state)).unwrap();
                }
            }
            SdlInput::Native(Event::GamepadButtonUp {
                which,
                button: sdl3::gamepad::Button::South,
                ..
            }) => {
                if let Some((_, state, output, _)) = pads.get_mut(&which) {
                    state.set(Button::South, false);
                    output.update(&mapper.map(state)).unwrap();
                }
            }
            SdlInput::Native(Event::GamepadRemoved { which, .. }) => {
                if let Some((_, _, output, index)) = pads.remove(&which) {
                    drop(output);
                    disconnected.send(index).unwrap();
                }
            }
            _ => {}
        }
    })
    .unwrap();
    let mut first = physical(0);
    assert_eq!(connection.recv_timeout(Duration::from_secs(5)).unwrap(), 0);
    let mut samples = Vec::new();
    for index in 0..1000 {
        let down = index % 2 == 0;
        let start = Instant::now();
        first
            .emit(&[InputEvent::new(
                EventType::KEY.0,
                KeyCode::BTN_SOUTH.0,
                i32::from(down),
            )])
            .unwrap();
        receive_button(&mut observers[0], down);
        samples.push(start.elapsed().as_nanos());
    }
    samples.sort_unstable();
    let p99 = samples[989];
    println!("{{\"samples\":1000,\"p99_us\":{}}}", p99 / 1000);
    assert!(
        p99 < 2_000_000,
        "input-to-kernel-report p99 was {} us",
        p99 / 1000
    );
    let mut second = physical(1);
    assert_eq!(connection.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
    second
        .emit(&[InputEvent::new(EventType::KEY.0, KeyCode::BTN_SOUTH.0, 1)])
        .unwrap();
    receive_button(&mut observers[1], true);
    let mut effect = observers[0]
        .upload_ff_effect(evdev::FFEffectData {
            direction: 0,
            trigger: Default::default(),
            replay: evdev::FFReplay {
                length: 100,
                delay: 0,
            },
            kind: evdev::FFEffectKind::Rumble {
                strong_magnitude: 12000,
                weak_magnitude: 34000,
            },
        })
        .unwrap();
    effect.play(1).unwrap();
    let value = feedback.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!((value.large, value.small), (12000, 34000));
    effect.stop().unwrap();
    let stopped = feedback.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!((stopped.large, stopped.small), (0, 0));
    drop(effect);
    drop(second);
    assert_eq!(
        disconnection.recv_timeout(Duration::from_secs(5)).unwrap(),
        1
    );
    drop(first);
    assert_eq!(
        disconnection.recv_timeout(Duration::from_secs(5)).unwrap(),
        0
    );
    input.stop();
}
