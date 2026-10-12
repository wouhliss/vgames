//! Dedicated SDL event thread. Idle has no Rust timer and shutdown pushes a
//! private SDL event. SDL handles and event pumping stay on their owner thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use sdl3::event::{Event, EventSender};

const DATABASE: &str = include_str!("../../resources/gamecontrollerdb.txt");

pub enum InputEvent {
    Native(Event),
    Wake,
}

struct Wake {
    sender: EventSender,
    event_type: u32,
}

impl Wake {
    fn push(&self) -> Result<(), sdl3::Error> {
        self.sender.push_event(Event::User {
            timestamp: 0,
            window_id: 0,
            type_: self.event_type,
            code: 0,
            data1: std::ptr::null_mut(),
            data2: std::ptr::null_mut(),
        })
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
                    });
                    let mut pump = sdl.event_pump().map_err(|error| error.to_string())?;
                    // Construct the handler here so it can own non-Send gamepad
                    // handles and is dropped before the SDL context, even on unwind.
                    let mut on_event = factory(&gamepads);
                    // Initial device notifications are dispatched before ready;
                    // the measured steady-state counter starts at zero.
                    for event in pump.poll_iter() {
                        on_event(InputEvent::Native(event), &gamepads);
                    }
                    ready.send(Ok(wake)).map_err(|error| error.to_string())?;
                    while !stop.load(Ordering::Acquire) {
                        let event = pump.wait_event();
                        count.fetch_add(1, Ordering::Relaxed);
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        if matches!(event, Event::User { type_, .. } if type_ == event_type) {
                            on_event(InputEvent::Wake, &gamepads);
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

    #[test]
    fn input_thread_starts_blocks_when_idle_and_wakes_for_shutdown() {
        let mut input = InputThread::start(|_| |_: InputEvent, _: &sdl3::GamepadSubsystem| {})
            .expect("SDL input starts");
        let before = input.iterations();
        // Keep the first startup wait out of the steady-state interval.
        #[cfg(target_os = "linux")]
        thread::sleep(Duration::from_millis(100));
        #[cfg(target_os = "linux")]
        let switches = input_thread_switches();
        thread::sleep(Duration::from_secs(10));
        assert_eq!(
            input.iterations(),
            before,
            "idle SDL wait returned without input"
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            input_thread_switches(),
            switches,
            "headless SDL woke internally while idle"
        );
        input.stop();
        assert!(input.iterations() > before, "shutdown did not wake SDL");
    }
}
