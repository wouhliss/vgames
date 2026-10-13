//! Xbox 360 reports through uinput, with event-driven force feedback. No SDL
//! handle leaves the input owner, and no feedback timer runs while idle.
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use super::linux_feedback::Feedback;

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode as Abs, AttributeSet, BusType, EventSummary, EventType, FFEffectCode,
    FFEffectKind, InputEvent, InputId, KeyCode as Key, UInputCode, UinputAbsSetup,
};

use super::{BackendError, EmulatedController, Rumble, RumbleSink, VirtualPad, VirtualPadBackend};
use crate::controllers::mapping::XReport;

pub const DEVICE_NAME: &str = "vgames Virtual Xbox 360";
const BUTTONS: [(Key, u16); 11] = [
    (Key::BTN_SOUTH, 0x1000),
    (Key::BTN_EAST, 0x2000),
    (Key::BTN_WEST, 0x4000),
    (Key::BTN_NORTH, 0x8000),
    (Key::BTN_SELECT, 0x0020),
    (Key::BTN_START, 0x0010),
    (Key::BTN_MODE, 0x0400),
    (Key::BTN_TL, 0x0100),
    (Key::BTN_TR, 0x0200),
    (Key::BTN_THUMBL, 0x0040),
    (Key::BTN_THUMBR, 0x0080),
];
pub struct Linux;
struct Pad {
    index: u16,
    device: Arc<Mutex<VirtualDevice>>,
    cancel: OwnedFd,
    feedback: Option<JoinHandle<()>>,
}
impl VirtualPadBackend for Linux {
    fn create(
        &self,
        target: EmulatedController,
        player_index: u16,
        rumble: RumbleSink,
    ) -> Result<Box<dyn VirtualPad>, BackendError> {
        if target != EmulatedController::Xbox360 {
            return Err(BackendError::UnsupportedTarget);
        }
        let keys: AttributeSet<Key> = BUTTONS.iter().map(|(key, _)| *key).collect();
        let mut builder = VirtualDevice::builder()?
            .name(DEVICE_NAME)
            .input_id(InputId::new(BusType::BUS_USB, 0x045e, 0x028e, 0x0114))
            .with_keys(&keys)?
            .with_ff(&AttributeSet::from_iter([FFEffectCode::FF_RUMBLE]))?
            .with_ff_effects_max(16);
        for axis in [Abs::ABS_X, Abs::ABS_Y, Abs::ABS_RX, Abs::ABS_RY] {
            builder = builder.with_absolute_axis(&UinputAbsSetup::new(
                axis,
                AbsInfo::new(0, -32768, 32767, 0, 0, 0),
            ))?;
        }
        for axis in [Abs::ABS_Z, Abs::ABS_RZ] {
            builder = builder
                .with_absolute_axis(&UinputAbsSetup::new(axis, AbsInfo::new(0, 0, 255, 0, 0, 0)))?;
        }
        for axis in [Abs::ABS_HAT0X, Abs::ABS_HAT0Y] {
            builder = builder
                .with_absolute_axis(&UinputAbsSetup::new(axis, AbsInfo::new(0, -1, 1, 0, 0, 0)))?;
        }
        let device = builder.build()?;
        nonblocking(device.as_raw_fd())?;
        let cancel = cancellation()?;
        let reader_cancel = cancel.try_clone()?;
        let device = Arc::new(Mutex::new(device));
        let reader = device.clone();
        let feedback = thread::Builder::new()
            .name("vgames-rumble".into())
            .spawn(move || {
                if let Err(error) = feedback_loop(reader, reader_cancel, rumble) {
                    tracing::warn!(%error, "virtual controller feedback stopped");
                }
            })?;
        Ok(Box::new(Pad {
            index: player_index,
            device,
            cancel,
            feedback: Some(feedback),
        }))
    }
}
impl VirtualPad for Pad {
    fn update(&mut self, report: &XReport) -> Result<(), BackendError> {
        if self
            .feedback
            .as_ref()
            .is_none_or(|worker| worker.is_finished())
        {
            return Err(BackendError::Stopped);
        }
        self.device
            .lock()
            .map_err(|_| BackendError::State)?
            .emit(&events(report))?;
        Ok(())
    }
    fn player_index(&self) -> u16 {
        self.index
    }
    fn node_path(&mut self) -> Result<Option<std::path::PathBuf>, BackendError> {
        let path = self
            .device
            .lock()
            .map_err(|_| BackendError::State)?
            .get_syspath()?;
        for entry in std::fs::read_dir(path)? {
            let name = entry?.file_name();
            if name
                .to_str()
                .and_then(|name| name.strip_prefix("event"))
                .is_some_and(|index| {
                    !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
                })
            {
                return Ok(Some(std::path::Path::new("/dev/input").join(name)));
            }
        }
        Ok(None)
    }
}
impl Drop for Pad {
    fn drop(&mut self) {
        signal(&self.cancel);
        if let Some(worker) = self.feedback.take()
            && worker.join().is_err()
        {
            tracing::warn!("virtual controller feedback panicked");
        }
    }
}
fn events(report: &XReport) -> [InputEvent; 19] {
    let mut events = [InputEvent::new(EventType::KEY.0, 0, 0); 19];
    for (event, (key, bit)) in events.iter_mut().zip(BUTTONS) {
        *event = InputEvent::new(
            EventType::KEY.0,
            key.0,
            i32::from(report.buttons & bit != 0),
        );
    }
    let values = [
        (Abs::ABS_X, i32::from(report.left.0)),
        (Abs::ABS_Y, -i32::from(report.left.1)),
        (Abs::ABS_RX, i32::from(report.right.0)),
        (Abs::ABS_RY, -i32::from(report.right.1)),
        (Abs::ABS_Z, i32::from(report.left_trigger)),
        (Abs::ABS_RZ, i32::from(report.right_trigger)),
        (
            Abs::ABS_HAT0X,
            i32::from(report.buttons & 8 != 0) - i32::from(report.buttons & 4 != 0),
        ),
        (
            Abs::ABS_HAT0Y,
            i32::from(report.buttons & 2 != 0) - i32::from(report.buttons & 1 != 0),
        ),
    ];
    for (event, (axis, value)) in events.iter_mut().skip(BUTTONS.len()).zip(values) {
        *event = InputEvent::new(EventType::ABSOLUTE.0, axis.0, value.clamp(-32768, 32767));
    }
    events
}
fn feedback_loop(
    device: Arc<Mutex<VirtualDevice>>,
    cancel: OwnedFd,
    emit: RumbleSink,
) -> Result<(), BackendError> {
    let fd = device.lock().map_err(|_| BackendError::State)?.as_raw_fd();
    let mut effects = Feedback::default();
    let mut previous = Rumble::default();
    loop {
        let now = Instant::now();
        let input = match wait_feedback(fd, &cancel, effects.next_deadline(now))? {
            Ready::Cancelled => return Ok(()),
            Ready::Input => true,
            Ready::Deadline => false,
        };
        if input {
            let mut device = device.lock().map_err(|_| BackendError::State)?;
            let pending: Vec<_> = match device.fetch_events() {
                Ok(events) => events.collect(),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                Err(error) => return Err(error.into()),
            };
            for event in pending {
                match event.destructure() {
                    EventSummary::UInput(event, UInputCode::UI_FF_UPLOAD, ..) => {
                        let mut upload = device.process_ff_upload(event)?;
                        let effect = upload.effect();
                        if let FFEffectKind::Rumble {
                            strong_magnitude,
                            weak_magnitude,
                        } = effect.kind
                        {
                            let id = effects.upload(
                                upload.effect_id(),
                                Rumble {
                                    large: strong_magnitude,
                                    small: weak_magnitude,
                                    duration_ms: u32::from(effect.replay.length),
                                    delay_ms: effect.replay.delay,
                                },
                            );
                            if let Some(id) = id {
                                upload.set_effect_id(id);
                                upload.set_retval(0);
                            } else {
                                upload.set_retval(-libc::ENOMEM);
                            }
                        } else {
                            upload.set_retval(-libc::EINVAL);
                        }
                    }
                    EventSummary::UInput(event, UInputCode::UI_FF_ERASE, ..) => {
                        let mut erase = device.process_ff_erase(event)?;
                        if let Ok(id) = i16::try_from(erase.effect_id()) {
                            effects.erase(id);
                        }
                        erase.set_retval(0);
                    }
                    EventSummary::ForceFeedback(_, id, value) => {
                        if let Ok(id) = i16::try_from(id.0) {
                            effects.play(id, value, Instant::now());
                        }
                    }
                    _ => {}
                }
            }
        }
        let output = effects.mix(Instant::now());
        if output != previous {
            emit(output);
            previous = output;
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
enum Ready {
    Input,
    Deadline,
    Cancelled,
}
fn wait_feedback(
    fd: std::os::fd::RawFd,
    cancel: &OwnedFd,
    deadline: Option<Instant>,
) -> Result<Ready, BackendError> {
    loop {
        let timeout = deadline.map_or(-1, |at| {
            i32::try_from(
                at.saturating_duration_since(Instant::now())
                    .as_millis()
                    .saturating_add(1),
            )
            .unwrap_or(i32::MAX)
        });
        let mut ready = [
            libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: cancel.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        #[allow(unsafe_code)]
        // SAFETY: poll borrows two live descriptors and writes only this initialized array.
        let result = unsafe { libc::poll(ready.as_mut_ptr(), 2, timeout) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if ready[1].revents != 0 {
            return Ok(Ready::Cancelled);
        }
        if ready[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(BackendError::Stopped);
        }
        return Ok(if result == 0 {
            Ready::Deadline
        } else {
            Ready::Input
        });
    }
}

fn nonblocking(fd: std::os::fd::RawFd) -> io::Result<()> {
    #[allow(unsafe_code)]
    // SAFETY: fcntl reads/updates flags on this owned live uinput descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(unsafe_code)]
    // SAFETY: preserve all existing status flags and add nonblocking I/O.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn cancellation() -> io::Result<OwnedFd> {
    #[allow(unsafe_code)]
    // SAFETY: eventfd returns a new descriptor; no pointers or borrowed payloads.
    let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(unsafe_code)]
    // SAFETY: successful eventfd is newly owned and closed once by OwnedFd.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
fn signal(fd: &OwnedFd) {
    let value = 1u64;
    loop {
        #[allow(unsafe_code)]
        // SAFETY: write reads one initialized u64 while the owned eventfd is live.
        let result = unsafe {
            libc::write(
                fd.as_raw_fd(),
                std::ptr::from_ref(&value).cast(),
                std::mem::size_of::<u64>(),
            )
        };
        if result >= 0 {
            return;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsupported_targets_are_refused_before_opening_a_kernel_device() {
        let result = Linux.create(EmulatedController::Dualshock4, 0, Arc::new(|_| {}));
        assert!(matches!(result, Err(BackendError::UnsupportedTarget)));
    }
    #[test]
    fn feedback_cancellation_is_latched_before_a_blocking_wait() {
        let (reader, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let cancel = cancellation().unwrap();
        signal(&cancel);
        assert_eq!(
            wait_feedback(reader.as_raw_fd(), &cancel, None).unwrap(),
            Ready::Cancelled
        );
    }
    #[test]
    fn reports_preserve_positions_triggers_hats_and_invert_y_without_overflow() {
        let report = XReport {
            buttons: 0x1000 | 0x4000 | 0x0001 | 0x0008,
            left: (-32768, -32768),
            right: (32767, 32767),
            left_trigger: 255,
            right_trigger: 128,
        };
        let event = events(&report);
        let value = |kind, code| {
            event
                .iter()
                .find(|event| event.event_type() == kind && event.code() == code)
                .unwrap()
                .value()
        };
        assert_eq!(value(EventType::KEY, Key::BTN_SOUTH.0), 1);
        assert_eq!(value(EventType::KEY, Key::BTN_WEST.0), 1);
        assert_eq!(value(EventType::KEY, Key::BTN_NORTH.0), 0);
        assert_eq!(value(EventType::ABSOLUTE, Abs::ABS_Y.0), 32767);
        assert_eq!(value(EventType::ABSOLUTE, Abs::ABS_RY.0), -32767);
        assert_eq!(value(EventType::ABSOLUTE, Abs::ABS_Z.0), 255);
        assert_eq!(value(EventType::ABSOLUTE, Abs::ABS_RZ.0), 128);
        assert_eq!(value(EventType::ABSOLUTE, Abs::ABS_HAT0X.0), 1);
        assert_eq!(value(EventType::ABSOLUTE, Abs::ABS_HAT0Y.0), -1);
    }
}
