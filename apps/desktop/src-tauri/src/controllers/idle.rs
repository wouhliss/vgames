//! Native notification wait used while no SDL gamepad is present. Each backend
//! observes device discovery and a latched private shutdown/control signal.

#[cfg(target_os = "linux")]
#[path = "input_linux.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "input_windows.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "input_macos.rs"]
mod platform;

pub use platform::{IdleWait, signal};
#[cfg(not(target_os = "linux"))]
pub use platform::Wake;
#[cfg(target_os = "linux")]
pub type Wake = std::os::fd::OwnedFd;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn signal_before_wait_is_latched_on_every_platform() {
        let wait = IdleWait::new().expect("native device monitor starts");
        let sender = wait.sender();
        let (done, received) = mpsc::channel();
        let fallback = thread::spawn(move || {
            if received.recv_timeout(Duration::from_secs(2)).is_ok() { return false; }
            signal(&sender).unwrap();
            true
        });
        signal(&wait.sender()).unwrap();
        wait.wait().unwrap();
        done.send(()).unwrap();
        assert!(!fallback.join().unwrap(), "a signal sent before waiting was lost");
    }
}
