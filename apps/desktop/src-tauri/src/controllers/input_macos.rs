//! An IOKit device notification source and a private Core Foundation source
//! provide a blocking headless wait. SDL discovers device changes after waking;
//! no Cocoa window, main-thread video subsystem or discovery timer is needed.

use std::ffi::{c_char, c_void};
use std::io;
use std::sync::Arc;

type Ref = *mut c_void;
#[repr(C)]
struct SourceContext {
    version: isize,
    info: Ref,
    retain: Option<unsafe extern "C" fn(Ref) -> Ref>,
    release: Option<unsafe extern "C" fn(Ref)>,
    description: Option<unsafe extern "C" fn(Ref) -> Ref>,
    equal: Option<unsafe extern "C" fn(Ref, Ref) -> u8>,
    hash: Option<unsafe extern "C" fn(Ref) -> usize>,
    schedule: Option<unsafe extern "C" fn(Ref, Ref, Ref)>,
    cancel: Option<unsafe extern "C" fn(Ref, Ref, Ref)>,
    perform: Option<unsafe extern "C" fn(Ref)>,
}

#[allow(unsafe_code)]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: Ref;
    fn CFRetain(value: Ref) -> Ref;
    fn CFRelease(value: Ref);
    fn CFRunLoopGetCurrent() -> Ref;
    fn CFRunLoopRun();
    fn CFRunLoopStop(run_loop: Ref);
    fn CFRunLoopWakeUp(run_loop: Ref);
    fn CFRunLoopAddSource(run_loop: Ref, source: Ref, mode: Ref);
    fn CFRunLoopRemoveSource(run_loop: Ref, source: Ref, mode: Ref);
    fn CFRunLoopSourceCreate(allocator: Ref, order: isize, context: *mut SourceContext) -> Ref;
    fn CFRunLoopSourceSignal(source: Ref);
    fn CFRunLoopSourceInvalidate(source: Ref);
}

#[allow(unsafe_code)]
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IONotificationPortCreate(main_port: u32) -> Ref;
    fn IONotificationPortDestroy(port: Ref);
    fn IONotificationPortGetRunLoopSource(port: Ref) -> Ref;
    fn IOServiceMatching(name: *const c_char) -> Ref;
    fn IOServiceAddMatchingNotification(
        port: Ref,
        notification: *const c_char,
        matching: Ref,
        callback: unsafe extern "C" fn(Ref, u32),
        context: Ref,
        iterator: *mut u32,
    ) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
}

pub struct Wake {
    run_loop: Ref,
    source: Ref,
}
#[allow(unsafe_code)]
// SAFETY: these retained CF objects support cross-thread SourceSignal/WakeUp.
// Running, registering, removing and invalidating sources stay on the owner.
unsafe impl Send for Wake {}
#[allow(unsafe_code)]
// SAFETY: CF's signal/wakeup operations synchronize accesses internally.
unsafe impl Sync for Wake {}

impl Drop for Wake {
    fn drop(&mut self) {
        #[allow(unsafe_code)]
        // SAFETY: release our retained source and loop exactly once. The source
        // has already been removed/invalidated on its owner thread.
        unsafe {
            CFRelease(self.source);
            CFRelease(self.run_loop);
        }
    }
}

pub struct IdleWait {
    wake: Arc<Wake>,
    port: Ref,
    notification_source: Ref,
    arrived: u32,
    removed: u32,
}

#[allow(unsafe_code)]
unsafe extern "C" fn stop(context: Ref) {
    // SAFETY: context is the retained owning run loop, valid until invalidation.
    unsafe {
        CFRunLoopStop(context);
    }
}

#[allow(unsafe_code)]
unsafe extern "C" fn changed(context: Ref, iterator: u32) {
    loop {
        // SAFETY: IOKit supplies its registered valid iterator. Each returned
        // service transfers one reference that is released immediately.
        let service = unsafe { IOIteratorNext(iterator) };
        if service == 0 {
            break;
        }
        unsafe {
            IOObjectRelease(service);
        }
    }
    // SAFETY: context is the retained owning loop; callback runs on that loop.
    unsafe {
        CFRunLoopStop(context);
    }
}

impl IdleWait {
    pub fn new() -> io::Result<Self> {
        #[allow(unsafe_code)]
        // SAFETY: retain the current thread's loop for the native callbacks and
        // cross-thread wake handle. Context has no borrowed Rust references.
        let (run_loop, source) = unsafe {
            let run_loop = CFRetain(CFRunLoopGetCurrent());
            let mut context = SourceContext {
                version: 0,
                info: run_loop,
                retain: None,
                release: None,
                description: None,
                equal: None,
                hash: None,
                schedule: None,
                cancel: None,
                perform: Some(stop),
            };
            let source = CFRunLoopSourceCreate(std::ptr::null_mut(), 0, &mut context);
            if source.is_null() {
                CFRelease(run_loop);
                return Err(io::Error::other("Cannot create controller wake source."));
            }
            CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode);
            (run_loop, source)
        };
        let wake = Arc::new(Wake { run_loop, source });
        #[allow(unsafe_code)]
        // SAFETY: zero chooses the default main IOKit port; result is owned.
        let port = unsafe { IONotificationPortCreate(0) };
        let mut wait = Self {
            wake,
            port,
            notification_source: std::ptr::null_mut(),
            arrived: 0,
            removed: 0,
        };
        if port.is_null() {
            return Err(io::Error::other("Cannot create controller device monitor."));
        }
        #[allow(unsafe_code)]
        // SAFETY: borrowed source remains valid until our port is destroyed.
        let notification_source = unsafe { IONotificationPortGetRunLoopSource(port) };
        if notification_source.is_null() {
            return Err(io::Error::other(
                "Cannot create controller notification source.",
            ));
        }
        wait.notification_source = notification_source;
        #[allow(unsafe_code)]
        // SAFETY: register the port's source on this owning retained loop.
        unsafe {
            CFRunLoopAddSource(run_loop, notification_source, kCFRunLoopDefaultMode);
        }
        wait.arrived = wait.register(c"IOServiceFirstMatch")?;
        wait.removed = wait.register(c"IOServiceTerminate")?;
        Ok(wait)
    }

    fn register(&self, notification: &std::ffi::CStr) -> io::Result<u32> {
        let mut iterator = 0;
        #[allow(unsafe_code)]
        // SAFETY: matching dictionary is consumed by IOKit on success/failure;
        // callback context stays retained through port destruction. Iterator
        // out-pointer is writable and both names are NUL terminated.
        let result = unsafe {
            let matching = IOServiceMatching(c"IOHIDDevice".as_ptr());
            if matching.is_null() {
                return Err(io::Error::other("Cannot match controller devices."));
            }
            IOServiceAddMatchingNotification(
                self.port,
                notification.as_ptr(),
                matching,
                changed,
                self.wake.run_loop,
                &mut iterator,
            )
        };
        if result != 0 {
            if iterator != 0 {
                #[allow(unsafe_code)]
                // SAFETY: release a partially returned owned iterator.
                unsafe {
                    IOObjectRelease(iterator);
                }
            }
            return Err(io::Error::other(format!(
                "Cannot register controller notification ({result})."
            )));
        }
        #[allow(unsafe_code)]
        // SAFETY: drain initial matches to arm delivery; the loop is not running
        // yet, so its Stop has no pending effect on the future blocking wait.
        unsafe {
            changed(self.wake.run_loop, iterator);
        }
        Ok(iterator)
    }

    pub fn sender(&self) -> Arc<Wake> {
        self.wake.clone()
    }

    pub fn wait(&self) -> io::Result<()> {
        #[allow(unsafe_code)]
        // SAFETY: called on the owner thread with two registered sources. A
        // queued signal is latched until its perform callback stops this run.
        unsafe {
            CFRunLoopRun();
        }
        Ok(())
    }
}

impl Drop for IdleWait {
    fn drop(&mut self) {
        #[allow(unsafe_code)]
        // SAFETY: remove/invalidate on owner thread, destroy delivery port before
        // releasing iterators and context. Cross-thread senders retain CF objects.
        unsafe {
            CFRunLoopRemoveSource(self.wake.run_loop, self.wake.source, kCFRunLoopDefaultMode);
            CFRunLoopSourceInvalidate(self.wake.source);
            if !self.notification_source.is_null() {
                CFRunLoopRemoveSource(
                    self.wake.run_loop,
                    self.notification_source,
                    kCFRunLoopDefaultMode,
                );
            }
            if !self.port.is_null() {
                IONotificationPortDestroy(self.port);
            }
            for iterator in [self.arrived, self.removed] {
                if iterator != 0 {
                    IOObjectRelease(iterator);
                }
            }
        }
    }
}

pub fn signal(wake: &Wake) -> io::Result<()> {
    #[allow(unsafe_code)]
    // SAFETY: both retained objects remain alive throughout these thread-safe
    // operations; Signal latches even when the loop has not entered Run yet.
    unsafe {
        CFRunLoopSourceSignal(wake.source);
        CFRunLoopWakeUp(wake.run_loop);
    }
    Ok(())
}
