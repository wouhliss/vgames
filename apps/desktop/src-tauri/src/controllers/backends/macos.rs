//! CoreHID stays disabled in unsigned builds. Inspect the running process's
//! restricted entitlement rather than trusting a config flag or environment.
use std::ffi::{c_char, c_void};

use super::{BackendError, EmulatedController, RumbleSink, VirtualPad, VirtualPadBackend};

pub struct Macos;
impl VirtualPadBackend for Macos {
    fn create(
        &self,
        _target: EmulatedController,
        _index: u16,
        _rumble: RumbleSink,
    ) -> Result<Box<dyn VirtualPad>, BackendError> {
        if !has_entitlement() {
            return Err(BackendError::EntitlementMissing);
        }
        // No restricted Swift bridge is distributed with our unsigned builds.
        Err(BackendError::Driver(
            "Virtual controller support is not included in this build.".into(),
        ))
    }
}
struct Owned(*const c_void);
impl Owned {
    fn new(value: *const c_void) -> Option<Self> {
        if value.is_null() {
            None
        } else {
            Some(Self(value))
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        #[allow(unsafe_code)]
        // SAFETY: every Owned comes from a successful Create/Copy CF API and
        // releases its single retained reference once.
        unsafe {
            CFRelease(self.0);
        }
    }
}
#[allow(unsafe_code)]
pub fn has_entitlement() -> bool {
    // SAFETY: null allocators select the default; this API creates a retained
    // task object for the current process, never an external/untrusted PID.
    let Some(task) = Owned::new(unsafe { SecTaskCreateFromSelf(std::ptr::null()) }) else {
        return false;
    };
    // SAFETY: the constant C string is terminated and alive for the call; CF
    // copies it using the documented UTF-8 encoding value.
    let Some(key) = Owned::new(unsafe {
        CFStringCreateWithCString(
            std::ptr::null(),
            c"com.apple.developer.hid.virtual.device".as_ptr(),
            0x0800_0100,
        )
    }) else {
        return false;
    };
    let mut error = std::ptr::null();
    // SAFETY: live retained task/string objects and an initialized error slot;
    // Copy returns owned CF objects, including a possible error.
    let value = Owned::new(unsafe { SecTaskCopyValueForEntitlement(task.0, key.0, &mut error) });
    let _error = Owned::new(error);
    let Some(value) = value else {
        return false;
    };
    // SAFETY: inspect the object's dynamic CF type before passing it to the
    // Boolean API; malformed or non-Boolean values fail closed.
    unsafe { CFGetTypeID(value.0) == CFBooleanGetTypeID() && CFBooleanGetValue(value.0) != 0 }
}
#[allow(unsafe_code)]
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecTaskCreateFromSelf(allocator: *const c_void) -> *const c_void;
    fn SecTaskCopyValueForEntitlement(
        task: *const c_void,
        key: *const c_void,
        error: *mut *const c_void,
    ) -> *const c_void;
}
#[allow(unsafe_code)]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        text: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFGetTypeID(value: *const c_void) -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(value: *const c_void) -> u8;
    fn CFRelease(value: *const c_void);
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn null_core_foundation_results_are_never_owned_or_released() {
        assert!(Owned::new(std::ptr::null()).is_none());
    }
    #[test]
    fn the_unsigned_process_cannot_create_a_corehid_device() {
        assert!(!has_entitlement());
        assert!(matches!(
            Macos.create(EmulatedController::Xbox360, 0, Arc::new(|_| {})),
            Err(BackendError::EntitlementMissing)
        ));
    }
}
