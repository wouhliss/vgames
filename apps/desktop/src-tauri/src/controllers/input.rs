//! Dedicated SDL event thread. With no pad, native device notifications block
//! without a discovery timer; shutdown pushes a private SDL event and signals
//! the native wait. SDL handles and event pumping stay on their owner thread.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use sdl3::event::{Event, EventSender};

use super::idle;

#[cfg(test)]
pub(crate) static SDL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const DATABASE: &str = include_str!("../../resources/gamecontrollerdb.txt");

pub enum InputEvent {
    Native(Event),
    Wake,
}

#[derive(Clone)]
pub struct InputWake(Arc<Wake>);

impl InputWake {
    pub fn wake(&self) -> Result<(), String> {
        self.0.push()
    }
}

struct Wake {
    sender: EventSender,
    event_type: u32,
    idle: Arc<idle::Wake>,
}

impl Wake {
    fn push(&self) -> Result<(), String> {
        let queued = self
            .sender
            .push_event(Event::User {
                timestamp: 0,
                window_id: 0,
                type_: self.event_type,
                code: 0,
                data1: std::ptr::null_mut(),
                data2: std::ptr::null_mut(),
            })
            .map_err(|error| error.to_string());
        // Signal after enqueueing: no callback-before-enqueue lost-wakeup race.
        // Even a full SDL queue must not prevent a cancelled native wait.
        idle::signal(&self.idle).map_err(|error| error.to_string())?;
        queued
    }
}

fn track_connection(connected: &mut BTreeSet<sdl3::joystick::JoystickId>, event: &Event) {
    match event {
        Event::GamepadAdded { which, .. } => {
            connected.insert(*which);
        }
        Event::GamepadRemoved { which, .. } => {
            connected.remove(which);
        }
        _ => {}
    }
}

/// Owner for the input worker. Callbacks execute on `vgames-sdl`, never on the UI
/// thread. Handlers are constructed there, may own gamepads on that thread and
/// must return promptly.
pub struct InputThread {
    wake: Arc<Wake>,
    stopped: Arc<AtomicBool>,
    iterations: Arc<AtomicUsize>,
    worker: Option<JoinHandle<()>>,
}

impl InputThread {
    pub fn start<F, H>(factory: F) -> Result<Self, String>
    where
        F: FnOnce(&sdl3::GamepadSubsystem) -> H + Send + 'static,
        H: FnMut(InputEvent, &sdl3::GamepadSubsystem) + 'static,
    {
        Self::start_with_wake(move |gamepads, _| factory(gamepads))
    }

    pub fn start_with_wake<F, H>(factory: F) -> Result<Self, String>
    where
        F: FnOnce(&sdl3::GamepadSubsystem, InputWake) -> H + Send + 'static,
        H: FnMut(InputEvent, &sdl3::GamepadSubsystem) + 'static,
    {
        let stopped = Arc::new(AtomicBool::new(false));
        let iterations = Arc::new(AtomicUsize::new(0));
        let stop = stopped.clone();
        let count = iterations.clone();
        let (ready, receive) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("vgames-sdl".into())
            .spawn(move || {
                let run = || -> Result<(), String> {
                    for hint in [
                        "SDL_JOYSTICK_HIDAPI",
                        "SDL_JOYSTICK_HIDAPI_PS4",
                        "SDL_JOYSTICK_HIDAPI_PS5",
                        "SDL_JOYSTICK_HIDAPI_SWITCH",
                        "SDL_JOYSTICK_HIDAPI_XBOX",
                        "SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS",
                    ] {
                        sdl3::hint::set(hint, "1");
                    }
                    let idle = idle::IdleWait::new().map_err(|error| error.to_string())?;
                    let sdl = sdl3::init().map_err(|error| error.to_string())?;
                    let gamepads = sdl.gamepad().map_err(|error| error.to_string())?;
                    gamepads
                        .load_mappings_from_read(&mut DATABASE.as_bytes())
                        .map_err(|error| error.to_string())?;
                    let events = sdl.event().map_err(|error| error.to_string())?;
                    #[allow(unsafe_code)]
                    // SAFETY: reserve one private SDL event id; every pushed event
                    // uses that id with null data pointers, never a borrowed payload.
                    let event_type =
                        unsafe { events.register_event() }.map_err(|error| error.to_string())?;
                    let wake = Arc::new(Wake {
                        sender: events.event_sender(),
                        event_type,
                        idle: idle.sender(),
                    });
                    let mut pump = sdl.event_pump().map_err(|error| error.to_string())?;
                    // Construct the handler here so it can own non-Send gamepad
                    // handles and is dropped before the SDL context, even on unwind.
                    let mut on_event = factory(&gamepads, InputWake(wake.clone()));
                    // Enumerate once: SDL enumeration allocates, so hotplug events
                    // maintain the connected set instead of allocating per input.
                    let mut connected: BTreeSet<_> = gamepads
                        .gamepads()
                        .map_err(|error| error.to_string())?
                        .into_iter()
                        .collect();
                    // Initial device notifications are dispatched before ready;
                    // the measured steady-state counter starts at zero.
                    for event in pump.poll_iter() {
                        track_connection(&mut connected, &event);
                        on_event(InputEvent::Native(event), &gamepads);
                    }
                    ready.send(Ok(wake)).map_err(|error| error.to_string())?;
                    while !stop.load(Ordering::Acquire) {
                        let event = if connected.is_empty() {
                            // Drain queued events before blocking. A device notification
                            // arriving during enumeration stays pending in the native monitor.
                            if let Some(event) = pump.poll_event() {
                                Some(event)
                            } else {
                                idle.wait().map_err(|error| error.to_string())?;
                                count.fetch_add(1, Ordering::Relaxed);
                                if stop.load(Ordering::Acquire) {
                                    break;
                                }
                                // Discovery can lazily establish the first mapping.
                                // Refresh only after a real native notification.
                                connected.extend(
                                    gamepads.gamepads().map_err(|error| error.to_string())?,
                                );
                                pump.poll_event()
                            }
                        } else {
                            Some(pump.wait_event())
                        };
                        let Some(event) = event else {
                            continue;
                        };
                        count.fetch_add(1, Ordering::Relaxed);
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        track_connection(&mut connected, &event);
                        if matches!(event, Event::User { type_, .. } if type_ == event_type) {
                            on_event(InputEvent::Wake, &gamepads);
                            // A control callback may attach the first software or
                            // virtual pad; ordinary input never enumerates devices.
                            if connected.is_empty() {
                                connected.extend(
                                    gamepads.gamepads().map_err(|error| error.to_string())?,
                                );
                            }
                        } else {
                            on_event(InputEvent::Native(event), &gamepads);
                        }
                    }
                    // A callback may own controller handles; release it before SDL.
                    drop(on_event);
                    Ok(())
                };
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        let _ = ready.send(Err(error));
                    }
                    Err(_) => {
                        let _ =
                            ready.send(Err("Controller input thread stopped unexpectedly.".into()));
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        match receive.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(wake)) => Ok(Self {
                wake,
                stopped,
                iterations,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(error) => {
                stopped.store(true, Ordering::Release);
                // Initialization cannot be interrupted safely inside SDL. Once
                // it returns, the worker observes stop before entering a wait.
                Err(error.to_string())
            }
        }
    }

    pub fn iterations(&self) -> usize {
        self.iterations.load(Ordering::Relaxed)
    }

    pub fn is_running(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
    }

    pub fn wake(&self) -> Result<(), String> {
        self.wake.push().map_err(|error| error.to_string())
    }

    pub fn stop(&mut self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Err(error) = self.wake.push() {
            tracing::warn!(%error, "cannot wake controller input for shutdown");
        }
        if let Some(worker) = self.worker.take()
            && worker.thread().id() != thread::current().id()
            && worker.join().is_err()
        {
            tracing::warn!("controller input thread panicked during shutdown");
        }
    }
}

impl Drop for InputThread {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    fn input_thread_switches() -> usize {
        for entry in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
            if std::fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|name| name.trim() == "vgames-sdl")
            {
                return std::fs::read_to_string(entry.path().join("status"))
                    .unwrap()
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("voluntary_ctxt_switches:")
                            .map(|value| value.trim().parse().unwrap())
                    })
                    .expect("thread context-switch counter");
            }
        }
        panic!("vgames-sdl thread not found");
    }

    /// Records what the idle wait watches (`/dev`, `/dev/input`) with the same
    /// mask, so an idle-test failure names the device-node change that woke it.
    #[cfg(target_os = "linux")]
    struct DevChanges(std::os::fd::OwnedFd);

    #[cfg(target_os = "linux")]
    impl DevChanges {
        fn start() -> Self {
            use std::os::fd::{AsRawFd, FromRawFd};
            #[allow(unsafe_code)]
            // SAFETY: inotify_init1 takes only flags and returns a fresh descriptor.
            let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
            assert!(fd >= 0, "{}", std::io::Error::last_os_error());
            #[allow(unsafe_code)]
            // SAFETY: fd was just created and is owned here.
            let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
            for path in [c"/dev", c"/dev/input"] {
                #[allow(unsafe_code)]
                // SAFETY: path is NUL terminated and fd is an owned inotify instance.
                unsafe {
                    libc::inotify_add_watch(
                        owned.as_raw_fd(),
                        path.as_ptr(),
                        libc::IN_CREATE
                            | libc::IN_DELETE
                            | libc::IN_ATTRIB
                            | libc::IN_MOVED_FROM
                            | libc::IN_MOVED_TO
                            | libc::IN_DELETE_SELF
                            | libc::IN_MOVE_SELF
                            | libc::IN_ONLYDIR,
                    );
                }
            }
            Self(owned)
        }

        /// Every queued change as `mask name`, oldest first.
        fn drain(&self) -> Vec<String> {
            use std::os::fd::AsRawFd;
            let mut seen = Vec::new();
            let mut bytes = [0_u8; 4096];
            loop {
                #[allow(unsafe_code)]
                // SAFETY: bytes is writable for its full size and the fd is owned.
                let read = unsafe {
                    libc::read(self.0.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len())
                };
                let Ok(read) = usize::try_from(read) else {
                    return seen;
                };
                let mut at = 0;
                // inotify_event: wd, mask, cookie, len (4 × u32), then `len` name bytes.
                while at + 16 <= read {
                    let field = |offset: usize| {
                        u32::from_ne_bytes(bytes[at + offset..at + offset + 4].try_into().unwrap())
                    };
                    let (mask, len) = (field(4), field(12) as usize);
                    let name = String::from_utf8_lossy(&bytes[at + 16..at + 16 + len]);
                    seen.push(format!("{mask:#x} {}", name.trim_end_matches('\0')));
                    at += 16 + len;
                }
            }
        }
    }

    #[test]
    fn input_thread_starts_blocks_when_idle_and_wakes_for_shutdown() {
        let _serial = SDL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        #[cfg(target_os = "linux")]
        let changes = DevChanges::start();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = events.clone();
        let mut input = InputThread::start(move |_| {
            move |event: InputEvent, _: &sdl3::GamepadSubsystem| {
                if let InputEvent::Native(event) = event {
                    seen.lock().unwrap().push(format!("{event:?}"));
                }
            }
        })
        .expect("SDL input starts");
        let before = input.iterations();
        // Keep the first startup wait out of the steady-state interval.
        #[cfg(target_os = "linux")]
        thread::sleep(Duration::from_millis(100));
        #[cfg(target_os = "linux")]
        let switches = input_thread_switches();
        #[cfg(target_os = "linux")]
        thread::sleep(Duration::from_secs(60));
        #[cfg(not(target_os = "linux"))]
        thread::sleep(Duration::from_secs(10));
        #[cfg(target_os = "linux")]
        let changes = changes.drain();
        #[cfg(not(target_os = "linux"))]
        let changes: Vec<String> = Vec::new();
        assert_eq!(
            input.iterations(),
            before,
            "idle SDL wait returned without input; SDL events: {:?}; /dev changes: {changes:?}",
            events.lock().unwrap()
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            input_thread_switches(),
            switches,
            "headless SDL woke internally while idle; /dev changes: {changes:?}"
        );
        input.stop();
        assert!(input.iterations() > before, "shutdown did not wake SDL");
    }
}
