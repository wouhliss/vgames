//! Exclude virtual outputs before treating devices as physical input. The
//! mapping database may rename a gamepad, so Linux uses the raw joystick name.
use super::launch::Bridge;
use sdl3::{GamepadSubsystem, joystick::JoystickId};

pub fn is_virtual(subsystem: &GamepadSubsystem, id: JoystickId, sessions: &Bridge) -> bool {
    #[cfg(target_os = "linux")]
    {
        let _ = (subsystem, sessions);
        raw_name(id).as_deref() == Some(super::backends::linux::DEVICE_NAME)
    }
    #[cfg(windows)]
    {
        if subsystem
            .path_for_id(id)
            .is_ok_and(|path| windows::virtual_bus(&path))
        {
            return true;
        }
        matches!(
            subsystem.real_type_for_id(id),
            sdl3::gamepad::GamepadType::Xbox360 | sdl3::gamepad::GamepadType::XboxOne
        ) && subsystem.player_index_for_id(id).is_some_and(|index| {
            sessions
                .book
                .lock()
                .is_ok_and(|book| book.owns_xinput_index(index))
        })
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (subsystem, id, sessions);
        false
    }
}
#[cfg(target_os = "linux")]
fn raw_name(id: JoystickId) -> Option<String> {
    #[allow(unsafe_code)]
    // SAFETY: called on SDL's input owner; SDL's borrowed NUL-terminated name
    // is copied immediately, before another SDL call or device removal.
    unsafe {
        let name = sdl3::sys::joystick::SDL_GetJoystickNameForID(
            sdl3::sys::joystick::SDL_JoystickID(id.raw()),
        );
        (!name.is_null()).then(|| {
            std::ffi::CStr::from_ptr(name)
                .to_string_lossy()
                .into_owned()
        })
    }
}
#[cfg(windows)]
#[path = "virtual_filter_windows.rs"]
mod windows;
