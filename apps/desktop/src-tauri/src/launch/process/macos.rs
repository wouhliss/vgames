//! macOS process groups tracked by kqueue. Fork notifications discover children
//! through libproc; a group scan also finds children after a fast leader exit.
//! The queue blocks without a timeout, and EVFILT_USER wakes it for cancellation.
//! Processes that leave the game's group are outside this tracker's scope.

use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{GameExit, ProcessError, ProcessIdentity};

const CANCEL_EVENT: usize = 1;
// Darwin libproc.h's process-group selector (not exported by our libc version).
const PROC_PGRP_ONLY: u32 = 2;
const MAX_PIDS: usize = 131_072;

pub(super) struct Game {
    identity: ProcessIdentity,
    queue: Arc<OwnedFd>,
    leader: Option<Child>,
    cancel: Canceller,
    killer: Killer,
    #[cfg(test)]
    wait_iterations: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Clone)]
pub(super) struct Canceller {
    queue: Arc<OwnedFd>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Clone)]
pub(super) struct Killer {
    identity: ProcessIdentity,
    finished: Arc<AtomicBool>,
}

#[derive(Clone, Copy)]
struct ProcessInfo {
    group: u32,
    started: u64,
    zombie: bool,
}

impl Game {
    pub(super) fn spawn(mut command: Command) -> Result<Self, ProcessError> {
        let child = command
            .process_group(0)
            .spawn()
            .map_err(ProcessError::Spawn)?;
        let pid = child.id();
        // The Child is not reaped yet, so even an exited leader retains its BSD identity.
        let info = process_info(pid)
            .map_err(ProcessError::Track)?
            .ok_or_else(|| ProcessError::Track(io::ErrorKind::NotFound.into()))?;
        Self::new(
            ProcessIdentity {
                pid,
                start_time: info.started,
            },
            Some(child),
        )
    }

    pub(super) fn reattach(identity: ProcessIdentity) -> Result<Option<Self>, ProcessError> {
        if let Some(info) = process_info(identity.pid).map_err(ProcessError::Track)?
            && (info.started != identity.start_time || info.group != identity.pid)
        {
            return Ok(None);
        }
        // BSD keeps a group's identity while members remain. The original PID
        // cannot be reassigned as a new process while it names that live group.
        if group_members(identity)
            .map_err(ProcessError::Track)?
            .is_empty()
        {
            return Ok(None);
        }
        Self::new(identity, None).map(Some)
    }

    fn new(identity: ProcessIdentity, leader: Option<Child>) -> Result<Self, ProcessError> {
        #[allow(unsafe_code)]
        // SAFETY: kqueue takes no arguments and returns a new descriptor, checked below.
        let descriptor = unsafe { libc::kqueue() };
        if descriptor < 0 {
            return Err(ProcessError::Track(io::Error::last_os_error()));
        }
        #[allow(unsafe_code)]
        // SAFETY: this successful kqueue descriptor is uniquely owned by us.
        let queue = Arc::new(unsafe { OwnedFd::from_raw_fd(descriptor) });
        #[allow(unsafe_code)]
        // SAFETY: set close-on-exec on an owned descriptor; fcntl has no pointer input.
        if unsafe { libc::fcntl(queue.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(ProcessError::Track(io::Error::last_os_error()));
        }
        let cancel_event = event(
            CANCEL_EVENT,
            libc::EVFILT_USER,
            libc::EV_ADD | libc::EV_CLEAR,
            0,
        );
        change(&queue, &cancel_event).map_err(ProcessError::Track)?;
        let cancel = Canceller {
            queue: queue.clone(),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        Ok(Self {
            identity,
            queue,
            leader,
            cancel,
            killer: Killer {
                identity,
                finished: Arc::new(AtomicBool::new(false)),
            },
            #[cfg(test)]
            wait_iterations: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    pub(super) fn identity(&self) -> ProcessIdentity {
        self.identity
    }
    pub(super) fn canceller(&self) -> Canceller {
        self.cancel.clone()
    }
    pub(super) fn killer(&self) -> Killer {
        self.killer.clone()
    }

    #[cfg(test)]
    pub(super) fn wait_counter(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.wait_iterations.clone()
    }

    pub(super) fn wait(mut self) -> Result<Option<GameExit>, ProcessError> {
        let mut watched = BTreeMap::new();
        let mut exit = None;
        loop {
            if self.cancel.cancelled.load(Ordering::Acquire) {
                return Ok(None);
            }
            if let Some(child) = &mut self.leader
                && let Some(status) = child.try_wait().map_err(ProcessError::Track)?
            {
                exit = Some(match (status.code(), status.signal()) {
                    (Some(code), _) => GameExit::Code(code),
                    (None, Some(signal)) => GameExit::Signal(signal),
                    _ => GameExit::Unknown,
                });
                self.leader = None;
            }
            // This scan runs only on startup and process notifications, never on a timer.
            let members = group_members(self.identity).map_err(ProcessError::Track)?;
            for (pid, started) in members {
                if watched.get(&pid) == Some(&started) {
                    continue;
                }
                if watch(&self.queue, pid, started).map_err(ProcessError::Track)? {
                    watched.insert(pid, started);
                }
            }
            let mut expired = Vec::new();
            for (&pid, &started) in &watched {
                if !process_info(pid)
                    .map_err(ProcessError::Track)?
                    .is_some_and(|info| {
                        !info.zombie && info.group == self.identity.pid && info.started == started
                    })
                {
                    expired.push(pid);
                }
            }
            for pid in expired {
                watched.remove(&pid);
            }
            if watched.is_empty() {
                // A fork may race watch registration or the leader's reaping.
                // Rescan before declaring the whole group exited.
                if !group_members(self.identity)
                    .map_err(ProcessError::Track)?
                    .is_empty()
                {
                    continue;
                }
                let leader = process_info(self.identity.pid).map_err(ProcessError::Track)?;
                if leader.is_some_and(|info| {
                    !info.zombie
                        && info.group == self.identity.pid
                        && info.started == self.identity.start_time
                }) {
                    return Err(ProcessError::Track(io::ErrorKind::InvalidData.into()));
                }
                // The leader may have exited between try_wait and the group scan.
                // Once its BSD identity is absent/zombie, reap it to retain its code.
                if leader.is_none_or(|info| info.zombie)
                    && let Some(mut child) = self.leader.take()
                {
                    let status = child.wait().map_err(ProcessError::Track)?;
                    exit = Some(match (status.code(), status.signal()) {
                        (Some(code), _) => GameExit::Code(code),
                        (None, Some(signal)) => GameExit::Signal(signal),
                        _ => GameExit::Unknown,
                    });
                }
                self.killer.finished.store(true, Ordering::Release);
                return Ok(Some(exit.unwrap_or(GameExit::Unknown)));
            }
            #[cfg(test)]
            self.wait_iterations.fetch_add(1, Ordering::Relaxed);
            let notifications = wait_events(&self.queue).map_err(ProcessError::Track)?;
            for notification in notifications {
                if notification.filter == libc::EVFILT_USER && notification.ident == CANCEL_EVENT {
                    return Ok(None);
                }
                if notification.flags & libc::EV_ERROR != 0 {
                    return Err(ProcessError::Track(io::Error::from_raw_os_error(
                        i32::try_from(notification.data).unwrap_or(libc::EIO),
                    )));
                }
                if notification.filter != libc::EVFILT_PROC {
                    continue;
                }
                let pid = u32::try_from(notification.ident)
                    .map_err(|_| ProcessError::Track(io::ErrorKind::InvalidData.into()))?;
                if notification.fflags & libc::NOTE_EXIT != 0 {
                    watched.remove(&pid);
                }
                if notification.fflags & libc::NOTE_FORK != 0 {
                    // macOS has NOTE_FORK but no NOTE_TRACK/NOTE_CHILD PID payload.
                    for child in child_pids(pid).map_err(ProcessError::Track)? {
                        if let Some(info) = process_info(child).map_err(ProcessError::Track)?
                            && !info.zombie
                            && info.group == self.identity.pid
                            && info.started >= self.identity.start_time
                            && watch(&self.queue, child, info.started)
                                .map_err(ProcessError::Track)?
                        {
                            watched.insert(child, info.started);
                        }
                    }
                }
            }
        }
    }
}

impl Canceller {
    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        let wake = event(CANCEL_EVENT, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER);
        if change(&self.queue, &wake).is_err() {
            tracing::warn!("cannot wake the macOS game tracker");
        }
    }
}

impl Killer {
    pub(super) fn terminate(&self, force: bool) -> Result<(), ProcessError> {
        if self.finished.load(Ordering::Acquire) {
            return Ok(());
        }
        if group_members(self.identity)
            .map_err(ProcessError::Track)?
            .is_empty()
        {
            return Ok(());
        }
        let group = i32::try_from(self.identity.pid)
            .map_err(|_| ProcessError::Track(io::ErrorKind::InvalidInput.into()))?;
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        #[allow(unsafe_code)]
        // SAFETY: a negative PID targets the verified live group, with no pointer inputs.
        let result = unsafe { libc::kill(-group, signal) };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(ProcessError::Track(error))
    }
}

fn process_info(pid: u32) -> io::Result<Option<ProcessInfo>> {
    let pid = i32::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size =
        i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).expect("BSD structure size");
    #[allow(unsafe_code)]
    // SAFETY: info is writable storage of the exact size required by PROC_PIDTBSDINFO.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read <= 0 {
        let error = io::Error::last_os_error();
        return if matches!(error.raw_os_error(), Some(libc::ESRCH | libc::ENOENT)) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    if read != size {
        return Err(io::ErrorKind::InvalidData.into());
    }
    #[allow(unsafe_code)]
    // SAFETY: proc_pidinfo returned the complete initialized BSD information structure.
    let info = unsafe { info.assume_init() };
    let started = info
        .pbi_start_tvsec
        .checked_mul(1_000_000)
        .and_then(|seconds| seconds.checked_add(info.pbi_start_tvusec))
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
    Ok(Some(ProcessInfo {
        group: info.pbi_pgid,
        started,
        zombie: info.pbi_status == libc::SZOMB,
    }))
}

fn event(ident: usize, filter: i16, flags: u16, fflags: u32) -> libc::kevent {
    libc::kevent {
        ident,
        filter,
        flags,
        fflags,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

fn change(queue: &OwnedFd, update: &libc::kevent) -> io::Result<()> {
    #[allow(unsafe_code)]
    // SAFETY: one live event is submitted to an owned queue; no output buffer is requested.
    let result = unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            update,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn watch(queue: &OwnedFd, pid: u32, started: u64) -> io::Result<bool> {
    let update = event(
        pid as usize,
        libc::EVFILT_PROC,
        libc::EV_ADD | libc::EV_CLEAR,
        libc::NOTE_EXIT | libc::NOTE_FORK,
    );
    if let Err(error) = change(queue, &update) {
        return if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(error)
        };
    }
    // Verify again after kernel registration to reject PID reuse during the call.
    Ok(process_info(pid)?.is_some_and(|info| info.started == started && !info.zombie))
}

fn wait_events(queue: &OwnedFd) -> io::Result<Vec<libc::kevent>> {
    let mut events = [event(0, 0, 0, 0); 64];
    loop {
        #[allow(unsafe_code)]
        // SAFETY: the owned queue writes into 64 live event slots. A null timeout
        // blocks indefinitely; EVFILT_USER provides the cancellation wakeup.
        let count = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                64,
                std::ptr::null(),
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
        let count =
            usize::try_from(count).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        return Ok(events[..count].to_vec());
    }
}

fn group_members(identity: ProcessIdentity) -> io::Result<BTreeMap<u32, u64>> {
    let mut members = BTreeMap::new();
    if process_info(identity.pid)?.is_some_and(|leader| leader.started != identity.start_time) {
        return Ok(members);
    }
    for pid in list_pids(identity.pid, false)? {
        if let Some(info) = process_info(pid)?
            && info.group == identity.pid
            && !info.zombie
            && info.started >= identity.start_time
        {
            members.insert(pid, info.started);
        }
    }
    Ok(members)
}

fn child_pids(pid: u32) -> io::Result<Vec<u32>> {
    list_pids(pid, true)
}

fn list_pids(pid: u32, children: bool) -> io::Result<Vec<u32>> {
    let signed = i32::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut capacity = 256;
    loop {
        let mut buffer = vec![0u32; capacity];
        let bytes = i32::try_from(buffer.len() * std::mem::size_of::<u32>())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        #[allow(unsafe_code)]
        // SAFETY: __error returns this thread's writable errno slot. Libproc
        // returns zero on errors as well as on empty results, so clear it first.
        unsafe {
            *libc::__error() = 0;
        }
        #[allow(unsafe_code)]
        // SAFETY: the libproc function writes at most bytes into our live PID buffer.
        let read = unsafe {
            if children {
                libc::proc_listchildpids(signed, buffer.as_mut_ptr().cast(), bytes)
            } else {
                libc::proc_listpids(PROC_PGRP_ONLY, pid, buffer.as_mut_ptr().cast(), bytes)
            }
        };
        if read < 0 {
            return Err(io::Error::last_os_error());
        }
        if read == 0 {
            let error = io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(0 | libc::ESRCH | libc::ENOENT)) {
                return Err(error);
            }
        }
        let read =
            usize::try_from(read).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        // proc_listchildpids returns a PID count; proc_listpids returns bytes.
        if !children && read % std::mem::size_of::<u32>() != 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let count = if children {
            read
        } else {
            read / std::mem::size_of::<u32>()
        };
        if count > buffer.len() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if count < buffer.len() {
            buffer.truncate(count);
            buffer.retain(|&pid| pid != 0);
            return Ok(buffer);
        }
        if capacity >= MAX_PIDS {
            return Err(io::Error::other(
                "Game process group exceeds the tracking limit",
            ));
        }
        capacity *= 2;
    }
}
