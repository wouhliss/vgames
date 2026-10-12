//! SDL HIDAPI creates a message-only device-notification window on its owner
//! thread. Wait for that thread's messages or a private event instead of SDL's
//! headless fallback pump. Dispatching notifications advances SDL's discovery.

use std::io;
use std::sync::Arc;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
    PeekMessageW, QS_ALLINPUT, TranslateMessage, WM_QUIT,
};

pub struct Wake(HANDLE);

#[allow(unsafe_code)]
// SAFETY: the event handle is owned, has no thread-affine payload and is only
// signalled cross-thread; Arc retains it through every waiter and sender.
unsafe impl Send for Wake {}
#[allow(unsafe_code)]
// SAFETY: Win32 event operations synchronize access to the kernel object.
unsafe impl Sync for Wake {}

impl Drop for Wake {
    fn drop(&mut self) {
        #[allow(unsafe_code)]
        // SAFETY: last owner closes its private event exactly once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub struct IdleWait {
    wake: Arc<Wake>,
}

impl IdleWait {
    pub fn new() -> io::Result<Self> {
        #[allow(unsafe_code)]
        // SAFETY: no security attributes/name, auto-reset, initially unset.
        let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            wake: Arc::new(Wake(handle)),
        })
    }

    pub fn sender(&self) -> Arc<Wake> {
        self.wake.clone()
    }

    pub fn wait(&self) -> io::Result<()> {
        #[allow(unsafe_code)]
        // SAFETY: a one-element borrowed live event array; messages belong to
        // this SDL owner thread. No timeout or background discovery timer.
        let result = unsafe {
            MsgWaitForMultipleObjectsEx(1, &self.wake.0, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };
        if result == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        if result == WAIT_OBJECT_0 {
            return Ok(());
        }
        if result != WAIT_OBJECT_0 + 1 {
            return Err(io::Error::other(
                "Unexpected controller message wait result.",
            ));
        }
        let mut message: MSG = Default::default();
        loop {
            #[allow(unsafe_code)]
            // SAFETY: message is writable; remove only this thread's messages.
            let found =
                unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) };
            if found == 0 {
                break;
            }
            if message.message == WM_QUIT {
                return Err(io::Error::other("Controller input message loop closed."));
            }
            #[allow(unsafe_code)]
            // SAFETY: dispatch the valid message on its owning thread. SDL's
            // native window procedure processes device arrivals/removals.
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }
}

pub fn signal(wake: &Wake) -> io::Result<()> {
    #[allow(unsafe_code)]
    // SAFETY: caller retains the owned event handle during SetEvent.
    if unsafe { SetEvent(wake.0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
