//! Linux tracking: the game runs in its own process group. The tracker holds a
//! pidfd per live member and sleeps in `poll(2)` until one exits, then rescans
//! `/proc` for members that are still alive. No timers. Processes that leave
//! the group (`setsid`, `setpgid`) are not tracked.

use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{GameExit, ProcessError, ProcessIdentity};

pub(super) struct Game {
    identity: ProcessIdentity,
    /// The launched process while it may still be alive; `Child` when this
    /// launcher run started it (so it can be reaped and its status read).
    leader: Option<(OwnedFd, Option<Child>)>,
    cancel: Canceller,
    killer: Killer,
}

#[derive(Clone)]
pub(super) struct Canceller(Arc<OwnedFd>);

#[derive(Clone)]
pub(super) struct Killer {
    pgid: i32,
    finished: Arc<AtomicBool>,
}

/// Fields of `/proc/<pid>/stat` the tracker needs.
struct Stat {
    zombie: bool,
    pgid: i32,
    start_time: u64,
}

impl Game {
    pub(super) fn spawn(mut command: Command) -> Result<Self, ProcessError> {
        let child = command
            .process_group(0)
            .spawn()
            .map_err(ProcessError::Spawn)?;
        let pid = child.id();
        let pidfd = pidfd_open(pid).map_err(ProcessError::Track)?;
        // Not reaped yet, so its stat entry exists even if it already exited.
        let stat =
            read_stat(pid).ok_or_else(|| ProcessError::Track(io::ErrorKind::NotFound.into()))?;
        Self::new(
            ProcessIdentity {
                pid,
                start_time: stat.start_time,
            },
            Some((pidfd, Some(child))),
        )
    }

    pub(super) fn reattach(identity: ProcessIdentity) -> Result<Option<Self>, ProcessError> {
        let ProcessIdentity { pid, start_time } = identity;
        let pgid = i32::try_from(pid)
            .map_err(|_| ProcessError::Track(io::ErrorKind::InvalidInput.into()))?;
        // Open first, then check the start time: the pidfd pins the process, so
        // a matching stat read afterwards cannot belong to a reused pid.
        let leader = match pidfd_open(pid) {
            Ok(fd) => match read_stat(pid) {
                Some(stat) if stat.start_time != start_time => return Ok(None),
                // An exited leader waits to be reaped: look for its group below.
                Some(stat) if stat.zombie => None,
                Some(_) => Some((fd, None)),
                None => None,
            },
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => None,
            Err(error) => return Err(ProcessError::Track(error)),
        };
        // The leader is gone: the game still runs if members of its group,
        // all started after it, remain. Linux never hands out a pid that is
        // still in use as a process group id.
        if leader.is_none() && group_members(pgid, start_time).is_empty() {
            return Ok(None);
        }
        Self::new(identity, leader).map(Some)
    }

    fn new(
        identity: ProcessIdentity,
        leader: Option<(OwnedFd, Option<Child>)>,
    ) -> Result<Self, ProcessError> {
        let pgid = i32::try_from(identity.pid)
            .map_err(|_| ProcessError::Track(io::ErrorKind::InvalidInput.into()))?;
        #[allow(unsafe_code)]
        // SAFETY: eventfd has no pointer arguments; the result is checked below.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(ProcessError::Track(io::Error::last_os_error()));
        }
        #[allow(unsafe_code)]
        // SAFETY: `fd` is a freshly created descriptor owned by nothing else.
        let cancel = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(Self {
            identity,
            leader,
            cancel: Canceller(Arc::new(cancel)),
            killer: Killer {
                pgid,
                finished: Arc::new(AtomicBool::new(false)),
            },
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

    pub(super) fn wait(mut self) -> Result<Option<GameExit>, ProcessError> {
        let mut exit = None;
        let mut leader = self.leader.take();
        let mut members: Vec<OwnedFd> = Vec::new();
        loop {
            if leader.is_none() && members.is_empty() {
                members = group_members(self.killer.pgid, self.identity.start_time);
                if members.is_empty() {
                    self.killer.finished.store(true, Ordering::Release);
                    return Ok(Some(exit.unwrap_or(GameExit::Unknown)));
                }
            }
            let mut fds: Vec<libc::pollfd> = std::iter::once(self.cancel.0.as_raw_fd())
                .chain(leader.as_ref().map(|(fd, _)| fd.as_raw_fd()))
                .chain(members.iter().map(AsRawFd::as_raw_fd))
                .map(|fd| libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                })
                .collect();
            poll(&mut fds).map_err(ProcessError::Track)?;
            let mut ready = fds.iter().map(|p| p.revents != 0);
            if ready.next() == Some(true) {
                return Ok(None);
            }
            // `take` drops a fired leader; only a leader this run started can be reaped.
            if leader.is_some()
                && ready.next() == Some(true)
                && let Some((_, Some(mut child))) = leader.take()
            {
                let status = child.wait().map_err(ProcessError::Track)?;
                exit = Some(match (status.code(), status.signal()) {
                    (Some(code), _) => GameExit::Code(code),
                    (None, Some(signal)) => GameExit::Signal(signal),
                    (None, None) => GameExit::Unknown,
                });
            }
            let ready: Vec<bool> = ready.collect();
            let mut index = 0;
            members.retain(|_| {
                let keep = !ready.get(index).copied().unwrap_or(false);
                index += 1;
                keep
            });
        }
    }
}

impl Canceller {
    pub(super) fn cancel(&self) {
        let one = 1u64.to_ne_bytes();
        #[allow(unsafe_code)]
        // SAFETY: writes 8 bytes from a live stack buffer to an eventfd we own.
        let _ = unsafe { libc::write(self.0.as_raw_fd(), one.as_ptr().cast(), one.len()) };
    }
}

impl Killer {
    pub(super) fn terminate(&self, force: bool) -> Result<(), ProcessError> {
        // After the group emptied its id may be reused: never signal it again.
        if self.finished.load(Ordering::Acquire) {
            return Ok(());
        }
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        #[allow(unsafe_code)]
        // SAFETY: kill has no pointer arguments; a negative pid targets the group.
        let result = unsafe { libc::kill(-self.pgid, signal) };
        let error = io::Error::last_os_error();
        match result {
            0 => Ok(()),
            _ if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
            _ => Err(ProcessError::Track(error)),
        }
    }
}

fn pidfd_open(pid: u32) -> io::Result<OwnedFd> {
    let pid =
        libc::pid_t::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    #[allow(unsafe_code)]
    // SAFETY: pidfd_open takes a pid and flags, no pointers; the result is checked.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = RawFd::try_from(fd).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    #[allow(unsafe_code)]
    // SAFETY: the kernel just returned this descriptor to us.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn poll(fds: &mut [libc::pollfd]) -> io::Result<()> {
    let count = libc::nfds_t::try_from(fds.len())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    loop {
        #[allow(unsafe_code)]
        // SAFETY: `fds` is a valid, exclusively borrowed slice of `count` entries.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), count, -1) };
        if result >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// pidfds of the live (non-zombie) processes in group `pgid` that started no
/// earlier than the game.
fn group_members(pgid: i32, not_before: u64) -> Vec<OwnedFd> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&pid| is_member(pid, pgid, not_before))
        .filter_map(|pid| {
            let fd = pidfd_open(pid).ok()?;
            // Re-check through the pinned process: the pid may have been reused
            // between the scan and the open.
            is_member(pid, pgid, not_before).then_some(fd)
        })
        .collect()
}

fn is_member(pid: u32, pgid: i32, not_before: u64) -> bool {
    read_stat(pid).is_some_and(|s| s.pgid == pgid && !s.zombie && s.start_time >= not_before)
}

fn read_stat(pid: u32) -> Option<Stat> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_stat(&text)
}

/// `pid (comm) state ppid pgrp … starttime(22nd field) …`; `comm` may contain
/// spaces and parentheses, so fields are counted after the last `)`.
fn parse_stat(text: &str) -> Option<Stat> {
    let (_, rest) = text.rsplit_once(')')?;
    let mut fields = rest.split_ascii_whitespace();
    let state = fields.next()?;
    let pgid = fields.nth(1)?.parse().ok()?;
    let start_time = fields.nth(16)?.parse().ok()?;
    Some(Stat {
        zombie: matches!(state, "Z" | "X" | "x"),
        pgid,
        start_time,
    })
}

#[cfg(test)]
#[test]
fn parses_stat_with_awkward_names() {
    let line =
        "4242 (a) b (c) S 1 4240 4240 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 1000 10";
    let stat = parse_stat(line).unwrap();
    assert_eq!(
        (stat.pgid, stat.start_time, stat.zombie),
        (4240, 987654, false)
    );
    assert!(
        parse_stat("1 (x) Z 0 7 7 0 -1 0 0 0 0 0 0 0 0 0 0 0 1 0 5")
            .unwrap()
            .zombie
    );
    assert!(parse_stat("1 (x) S 0").is_none());
}
