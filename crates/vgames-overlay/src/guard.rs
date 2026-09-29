//! The hook safety net (05-social §6): every hook body runs through [`guarded`]. A panic
//! inside is caught at the hook boundary and turns the overlay off for the rest of the
//! process; the game never sees it. Once off, every hook returns at once (one atomic load).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};

static DISABLED: AtomicBool = AtomicBool::new(false);

/// `true` after a hook panicked (or [`disable`] was called).
#[inline]
pub fn is_disabled() -> bool {
    DISABLED.load(Ordering::Relaxed)
}

/// Turns the overlay off for this process.
pub fn disable() {
    DISABLED.store(true, Ordering::Relaxed);
}

/// Runs `f` unless the overlay is off. A panic disables the overlay and returns `None`.
#[inline]
pub fn guarded<R>(f: impl FnOnce() -> R) -> Option<R> {
    if is_disabled() {
        return None;
    }
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => Some(r),
        Err(_) => {
            disable();
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_in_a_hook_disables_the_overlay_without_unwinding_into_the_caller() {
        // Quiet the default hook for this expected panic.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        assert_eq!(guarded(|| 1 + 1), Some(2));
        let r: Option<()> = guarded(|| panic!("boom in Present"));
        std::panic::set_hook(prev);
        assert_eq!(r, None);
        assert!(is_disabled());
        // Every later hook call is a no-op.
        let mut ran = false;
        assert_eq!(guarded(|| ran = true), None);
        assert!(!ran);
    }
}
