//! Headless SDL has no native wait implementation on Linux. With no gamepad,
//! wait on device-node changes and our private eventfd instead of its 1 ms pump.
//! Watch /dev before enumeration, including directory creation and permission
//! changes: containers often start without /dev/input and udev grants access later.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;

pub struct IdleWait {
    notify: OwnedFd,
    wake: Arc<OwnedFd>,
    input: std::ffi::CString,
}

fn owned(fd: libc::c_int) -> io::Result<OwnedFd> {
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(unsafe_code)]
    // SAFETY: a successful creation returns a fresh descriptor owned here.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

impl IdleWait {
    pub fn new() -> io::Result<Self> {
        Self::at(std::path::Path::new("/dev"))
    }

    fn at(root: &std::path::Path) -> io::Result<Self> {
        use std::os::unix::ffi::OsStrExt;
        let root_name = std::ffi::CString::new(root.as_os_str().as_bytes())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let input = std::ffi::CString::new(root.join("input").as_os_str().as_bytes())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        #[allow(unsafe_code)]
        // SAFETY: these creation functions take only flags and return fresh fds.
        let (notify, wake) = unsafe {
            (
                owned(libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK))?,
                owned(libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK))?,
            )
        };
        let wait = Self {
            notify,
            wake: Arc::new(wake),
            input,
        };
        wait.watch(&root_name, false)?;
        wait.watch(&wait.input, true)?;
        Ok(wait)
    }

    fn watch(&self, path: &std::ffi::CStr, optional: bool) -> io::Result<()> {
        #[allow(unsafe_code)]
        // SAFETY: path is NUL terminated and fd is an owned inotify instance.
        let result = unsafe {
            libc::inotify_add_watch(
                self.notify.as_raw_fd(),
                path.as_ptr(),
                libc::IN_CREATE
                    | libc::IN_DELETE
                    | libc::IN_ATTRIB
                    | libc::IN_MOVED_FROM
                    | libc::IN_MOVED_TO
                    | libc::IN_DELETE_SELF
                    | libc::IN_MOVE_SELF
                    | libc::IN_ONLYDIR,
            )
        };
        if result >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if optional && error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(());
        }
        Err(error)
    }

    pub fn sender(&self) -> Arc<OwnedFd> {
        self.wake.clone()
    }

    pub fn wait(&self) -> io::Result<()> {
        let mut fds = [
            libc::pollfd {
                fd: self.notify.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            #[allow(unsafe_code)]
            // SAFETY: fds is a valid writable array, descriptors stay owned,
            // and -1 blocks indefinitely; the eventfd always permits shutdown.
            let result = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
            if result >= 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        for fd in &fds {
            if fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Err(io::Error::other("Controller device monitor closed."));
            }
            if fd.revents & libc::POLLIN != 0 {
                drain(fd.fd)?;
            }
        }
        // A newly created or replaced input directory needs its own watch.
        self.watch(&self.input, true)?;
        Ok(())
    }
}

pub fn signal(fd: &OwnedFd) -> io::Result<()> {
    let value = 1_u64;
    loop {
        #[allow(unsafe_code)]
        // SAFETY: value is a readable eight-byte eventfd value, fd stays owned.
        let result = unsafe { libc::write(fd.as_raw_fd(), std::ptr::from_ref(&value).cast(), 8) };
        if result == 8 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            return Ok(());
        }
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn drain(fd: libc::c_int) -> io::Result<()> {
    // inotify records need a buffer large enough for at least one NAME_MAX
    // entry; eventfd uses exactly eight bytes regardless of the larger buffer.
    let mut bytes = [0_u8; 4096];
    loop {
        #[allow(unsafe_code)]
        // SAFETY: bytes is writable for its full size and fd is still owned.
        let result = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        if result > 0 {
            continue;
        }
        if result == 0 {
            return Err(io::Error::other("Controller device monitor reached EOF."));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            return Ok(());
        }
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    /// Starts a blocking wait on a worker thread once it is idle. The kernel can deliver an
    /// event of the previous step late (removing a watched directory queues `IN_IGNORED` only
    /// when its inode is evicted), which wakes a wait before this step's action: such a wake is
    /// drained and the wait started again. It must then block within a few tries.
    fn blocked(mut wait: IdleWait) -> (thread::JoinHandle<IdleWait>, mpsc::Receiver<()>) {
        for _ in 0..5 {
            let (ready, receive) = mpsc::channel();
            let worker = thread::spawn(move || {
                wait.wait().unwrap();
                ready.send(()).unwrap();
                wait
            });
            if receive.recv_timeout(Duration::from_millis(50)).is_err() {
                return (worker, receive);
            }
            wait = worker.join().unwrap();
        }
        panic!("wait did not block");
    }

    fn notification(wait: IdleWait, action: impl FnOnce()) -> IdleWait {
        let cancel = wait.sender();
        let (worker, receive) = blocked(wait);
        action();
        let detected = receive.recv_timeout(Duration::from_secs(2)).is_ok();
        if !detected {
            signal(&cancel).unwrap();
        }
        let wait = worker.join().unwrap();
        assert!(
            detected,
            "device notification did not wake the blocking wait"
        );
        wait
    }

    #[test]
    fn device_directory_creation_permissions_and_replacement_wake_without_polling() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let wait = IdleWait::at(root.path()).unwrap();
        // Start without /dev/input, as in a container before the first pad.
        let input = root.path().join("input");
        let wait = notification(wait, || std::fs::create_dir(&input).unwrap());
        let node = input.join("event0");
        let wait = notification(wait, || std::fs::write(&node, []).unwrap());
        let wait = notification(wait, || {
            std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o600)).unwrap()
        });
        let wait = notification(wait, || std::fs::remove_file(&node).unwrap());
        let wait = notification(wait, || std::fs::remove_dir(&input).unwrap());
        let wait = notification(wait, || std::fs::create_dir(&input).unwrap());
        let _wait = notification(wait, || std::fs::write(&node, []).unwrap());
    }

    #[test]
    fn shutdown_signal_before_wait_is_latched() {
        let root = tempfile::tempdir().unwrap();
        let wait = IdleWait::at(root.path()).unwrap();
        signal(&wait.sender()).unwrap();
        wait.wait().unwrap();
        let sender = wait.sender();
        let _wait = notification(wait, || signal(&sender).unwrap());
    }
}
