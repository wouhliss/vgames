//! Input idle time from the operating system, for `away` presence (05-social §3).
//!
//! Windows: `GetLastInputInfo`. macOS: `CGEventSourceSecondsSinceLastEventType`. Linux has
//! no single API (X11 screensaver extension, per-compositor Wayland protocols), so it
//! reports nothing and the user is never marked away automatically there.

use std::time::Duration;

#[cfg(windows)]
pub fn system_idle() -> Option<Duration> {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

    let mut info = LASTINPUTINFO {
        cbSize: u32::try_from(std::mem::size_of::<LASTINPUTINFO>()).ok()?,
        dwTime: 0,
    };
    // SAFETY: `info` is a valid, initialized LASTINPUTINFO with `cbSize` set, as the API
    // requires; both calls only read process-independent counters.
    #[allow(unsafe_code)]
    let (ok, now) = unsafe { (GetLastInputInfo(&mut info), GetTickCount()) };
    if ok == 0 {
        return None;
    }
    // Both counters wrap every ~49.7 days; wrapping subtraction keeps the difference right.
    Some(Duration::from_millis(u64::from(
        now.wrapping_sub(info.dwTime),
    )))
}

#[cfg(target_os = "macos")]
pub fn system_idle() -> Option<Duration> {
    // SAFETY: the signature matches CoreGraphics' declaration
    // (`CGEventSourceStateID`, `CGEventType` → `CFTimeInterval`).
    #[allow(unsafe_code)]
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    }
    /// `kCGEventSourceStateCombinedSessionState`.
    const COMBINED_SESSION_STATE: i32 = 0;
    /// `kCGAnyInputEventType`.
    const ANY_INPUT_EVENT: u32 = u32::MAX;
    // SAFETY: a pure query taking two plain integers and returning a double.
    #[allow(unsafe_code)]
    let seconds =
        unsafe { CGEventSourceSecondsSinceLastEventType(COMBINED_SESSION_STATE, ANY_INPUT_EVENT) };
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds))
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn system_idle() -> Option<Duration> {
    None
}
