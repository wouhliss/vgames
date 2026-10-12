//! Windows trees run in unnamed Job Objects. Closing our handles never kills
//! a running game. The tracker sleeps in the job's completion port, and cancel
//! posts a separate completion packet rather than polling.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, FILETIME, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED, PostQueuedCompletionStatus,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
    JOBOBJECT_ASSOCIATE_COMPLETION_PORT, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JobObjectAssociateCompletionPortInformation, JobObjectBasicAccountingInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::SystemServices::JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateEventW, CreateProcessW, GetExitCodeProcess,
    GetProcessTimes, INFINITE, OpenProcess, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SET_QUOTA, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, ResumeThread, STARTUPINFOW,
    SetEvent, TerminateProcess, WaitForMultipleObjects, WaitForSingleObject,
};

use super::{GameExit, ProcessError, ProcessIdentity, SuspendedProcess, windows_args};
use crate::launch::PreparedLaunch;

const JOB_KEY: usize = 1;
const CANCEL_KEY: usize = 2;
const STOP_CODE: u32 = 1;

pub(super) struct Game {
    identity: ProcessIdentity,
    process: Arc<OwnedHandle>,
    thread: Option<Arc<OwnedHandle>>,
    job: Arc<OwnedHandle>,
    killable_job: Arc<Mutex<Option<Arc<OwnedHandle>>>>,
    port: Arc<OwnedHandle>,
    cancel_event: Arc<OwnedHandle>,
    cancelled: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    reattached: bool,
    #[cfg(test)]
    final_wait_entered: Arc<AtomicBool>,
}

#[derive(Clone)]
pub(super) struct Canceller {
    port: Arc<OwnedHandle>,
    event: Arc<OwnedHandle>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Clone)]
pub(super) struct Killer {
    job: Arc<Mutex<Option<Arc<OwnedHandle>>>>,
    finished: Arc<AtomicBool>,
}

impl Game {
    pub(super) fn spawn(launch: &PreparedLaunch) -> Result<Self, ProcessError> {
        let mut game = Self::spawn_suspended(launch)?;
        game.resume()?;
        Ok(game)
    }

    pub(super) fn spawn_suspended(launch: &PreparedLaunch) -> Result<Self, ProcessError> {
        let program =
            windows_args::wide(launch.program.as_os_str()).map_err(ProcessError::Spawn)?;
        let directory =
            windows_args::wide(launch.working_dir.as_os_str()).map_err(ProcessError::Spawn)?;
        let mut command = windows_args::command_line(launch).map_err(ProcessError::Spawn)?;
        let environment = windows_args::environment(launch).map_err(ProcessError::Spawn)?;
        let (job, port, cancel_event) = new_job().map_err(ProcessError::Track)?;
        let startup = STARTUPINFOW {
            cb: u32::try_from(std::mem::size_of::<STARTUPINFOW>()).expect("Win32 structure size"),
            ..Default::default()
        };
        let mut info = PROCESS_INFORMATION::default();
        #[allow(unsafe_code)]
        // SAFETY: all strings/buffers are terminated and live for this call; info
        // is writable. Handles are not inherited. The application path is explicit.
        let ok = unsafe {
            CreateProcessW(
                program.as_ptr(),
                command.as_mut_ptr(),
                null(),
                null(),
                0,
                CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
                environment.as_ptr().cast(),
                directory.as_ptr(),
                &startup,
                &mut info,
            )
        };
        if ok == 0 {
            return Err(ProcessError::Spawn(io::Error::last_os_error()));
        }
        let process = Arc::new(own(info.hProcess).map_err(ProcessError::Track)?);
        let thread = Arc::new(own(info.hThread).map_err(ProcessError::Track)?);
        // Construct the guard before any fallible setup, so an error cannot strand
        // a suspended process. Only a not-yet-resumed game is killed on drop.
        let mut game = Self {
            identity: ProcessIdentity {
                pid: info.dwProcessId,
                start_time: 0,
            },
            process,
            thread: Some(thread),
            killable_job: Arc::new(Mutex::new(Some(job.clone()))),
            job,
            port,
            cancel_event,
            cancelled: Arc::new(AtomicBool::new(false)),
            finished: Arc::new(AtomicBool::new(false)),
            reattached: false,
            #[cfg(test)]
            final_wait_entered: Arc::new(AtomicBool::new(false)),
        };
        game.identity.start_time = creation_time(&game.process).map_err(ProcessError::Track)?;
        assign(&game.job, &game.process).map_err(ProcessError::Track)?;
        Ok(game)
    }

    pub(super) fn suspended(&self) -> Option<SuspendedProcess> {
        Some(SuspendedProcess {
            identity: self.identity,
            process: self.process.clone(),
            thread: self.thread.as_ref()?.clone(),
        })
    }

    pub(super) fn resume(&mut self) -> Result<(), ProcessError> {
        let Some(thread) = self.thread.as_ref() else {
            return Ok(());
        };
        loop {
            #[allow(unsafe_code)]
            // SAFETY: this handle owns the primary thread created with CREATE_SUSPENDED.
            // Drain any extra suspensions a failed hook left behind too.
            let previous = unsafe { ResumeThread(raw(thread)) };
            if previous == u32::MAX {
                return Err(ProcessError::Track(io::Error::last_os_error()));
            }
            if previous <= 1 {
                break;
            }
        }
        self.thread = None;
        Ok(())
    }

    pub(super) fn reattach(identity: ProcessIdentity) -> Result<Option<Self>, ProcessError> {
        #[allow(unsafe_code)]
        // SAFETY: OpenProcess takes a PID and flags; the returned handle is checked.
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                identity.pid,
            )
        };
        if handle.is_null() {
            let error = io::Error::last_os_error();
            // A terminated process being deleted can return ACCESS_DENIED, not
            // INVALID_PARAMETER. Confirm it has no live snapshot entry;
            // never hide an access denial for a still-running process.
            let absent = error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32)
                || (error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32)
                    && !process_parents()
                        .map_err(ProcessError::Track)?
                        .contains_key(&identity.pid));
            return if absent {
                Ok(None)
            } else {
                Err(track_error("open re-attach query handle", error))
            };
        }
        let process = Arc::new(own(handle).map_err(ProcessError::Track)?);
        #[allow(unsafe_code)]
        // SAFETY: the owned process handle supports synchronization; zero never blocks.
        let state = unsafe { WaitForSingleObject(raw(&process), 0) };
        if state == 0 {
            return Ok(None);
        }
        if state == u32::MAX {
            return Err(ProcessError::Track(io::Error::last_os_error()));
        }
        if creation_time(&process).map_err(|error| track_error("read re-attach identity", error))?
            != identity.start_time
        {
            return Ok(None);
        }
        // Only a verified live process needs the mutation rights required for
        // assignment. Windows can deny SET_QUOTA for an exited process even
        // while it remains visible in a snapshot and can still be queried.
        #[allow(unsafe_code)]
        // SAFETY: open a second owned handle to the validated PID, without inheritance.
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION
                    | PROCESS_SYNCHRONIZE
                    | PROCESS_SET_QUOTA
                    | PROCESS_TERMINATE,
                0,
                identity.pid,
            )
        };
        if handle.is_null() {
            let error = io::Error::last_os_error();
            #[allow(unsafe_code)]
            // SAFETY: the original query handle pins this identity; zero never blocks.
            if unsafe { WaitForSingleObject(raw(&process), 0) } == 0 {
                return Ok(None);
            }
            return Err(track_error("open re-attach assignment handle", error));
        }
        let process = Arc::new(own(handle).map_err(ProcessError::Track)?);
        if creation_time(&process).map_err(ProcessError::Track)? != identity.start_time {
            return Ok(None);
        }
        let (job, port, cancel_event) = new_job().map_err(ProcessError::Track)?;
        assign(&job, &process).map_err(|error| track_error("assign re-attach job", error))?;
        // New descendants inherit this job; existing descendants must be adopted too.
        adopt_children(&job, identity)
            .map_err(|error| track_error("adopt re-attach children", error))?;
        Ok(Some(Self {
            identity,
            process,
            thread: None,
            killable_job: Arc::new(Mutex::new(Some(job.clone()))),
            job,
            port,
            cancel_event,
            cancelled: Arc::new(AtomicBool::new(false)),
            finished: Arc::new(AtomicBool::new(false)),
            reattached: true,
            #[cfg(test)]
            final_wait_entered: Arc::new(AtomicBool::new(false)),
        }))
    }

    pub(super) fn identity(&self) -> ProcessIdentity {
        self.identity
    }

    pub(super) fn canceller(&self) -> Canceller {
        Canceller {
            port: self.port.clone(),
            event: self.cancel_event.clone(),
            cancelled: self.cancelled.clone(),
        }
    }

    pub(super) fn killer(&self) -> Killer {
        Killer {
            job: self.killable_job.clone(),
            finished: self.finished.clone(),
        }
    }

    pub(super) fn wait(self) -> Result<Option<GameExit>, ProcessError> {
        if self.thread.is_some() {
            return Err(ProcessError::Track(io::ErrorKind::InvalidInput.into()));
        }
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return Ok(None);
            }
            // Completion packets can precede our first wait, and Windows documents
            // job notifications as a hint. Confirm every zero message with accounting.
            if active_processes(&self.job).map_err(ProcessError::Track)? == 0 {
                // Job accounting/notifications can reach zero before the
                // process object finishes termination. Confirm the leader's
                // signaled state before exit callbacks or further re-attach.
                let handles = [raw(&self.process), raw(&self.cancel_event)];
                #[cfg(test)]
                self.final_wait_entered.store(true, Ordering::Release);
                #[allow(unsafe_code)]
                // SAFETY: both owned synchronization handles remain live;
                // cancellation wakes this indefinite wait through SetEvent.
                let state = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
                if state == 1 || self.cancelled.load(Ordering::Acquire) {
                    return Ok(None);
                }
                if state == u32::MAX {
                    return Err(ProcessError::Track(io::Error::last_os_error()));
                }
                if state != 0 {
                    return Err(ProcessError::Track(io::ErrorKind::InvalidData.into()));
                }
                self.finished.store(true, Ordering::Release);
                // Finished stop handles no longer need to pin the job (and
                // its terminated process objects). Active/detached sessions
                // retain it, so cancellation never removes their stop handle.
                self.killable_job
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                if self.reattached {
                    return Ok(Some(GameExit::Unknown));
                }
                let mut code = 0;
                #[allow(unsafe_code)]
                // SAFETY: the process handle remains owned; code is writable storage.
                let ok = unsafe { GetExitCodeProcess(raw(&self.process), &mut code) };
                if ok == 0 {
                    return Err(ProcessError::Track(io::Error::last_os_error()));
                }
                return Ok(Some(GameExit::Code(i32::from_ne_bytes(code.to_ne_bytes()))));
            }
            let mut message = 0;
            let mut key = 0;
            let mut packet: *mut OVERLAPPED = null_mut();
            #[allow(unsafe_code)]
            // SAFETY: port is owned, output pointers are live; packet is an opaque
            // completion payload and is never dereferenced. INFINITE has no polling.
            let ok = unsafe {
                GetQueuedCompletionStatus(
                    raw(&self.port),
                    &mut message,
                    &mut key,
                    &mut packet,
                    INFINITE,
                )
            };
            if ok == 0 {
                return Err(ProcessError::Track(io::Error::last_os_error()));
            }
            if key == CANCEL_KEY {
                return Ok(None);
            }
            if key == JOB_KEY && message == JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO {
                // Recheck accounting on the next iteration.
                continue;
            }
        }
    }
}

impl Drop for Game {
    fn drop(&mut self) {
        if self.thread.is_some() {
            #[allow(unsafe_code)]
            // SAFETY: this owned process has not been resumed; no game code ran.
            unsafe {
                TerminateProcess(raw(&self.process), STOP_CODE);
            }
        }
    }
}

impl Canceller {
    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        #[allow(unsafe_code)]
        // SAFETY: the owned manual-reset event wakes the final process wait.
        if unsafe { SetEvent(raw(&self.event)) } == 0 {
            tracing::warn!("cannot signal Windows game tracker cancellation");
        }
        #[allow(unsafe_code)]
        // SAFETY: posts an opaque packet to our owned port; no pointer is dereferenced.
        let ok = unsafe { PostQueuedCompletionStatus(raw(&self.port), 0, CANCEL_KEY, null()) };
        if ok == 0 {
            tracing::warn!("cannot wake the Windows game tracker");
        }
    }
}

impl Killer {
    pub(super) fn terminate(&self, _force: bool) -> Result<(), ProcessError> {
        if self.finished.load(Ordering::Acquire) {
            return Ok(());
        }
        let job = self
            .job
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let Some(job) = job else {
            return Ok(());
        };
        // Windows has no SIGTERM equivalent for an arbitrary GUI process tree.
        // The caller already obtained the player's stop confirmation.
        #[allow(unsafe_code)]
        // SAFETY: our job handle pins this tree even after a PID is recycled.
        let ok = unsafe { TerminateJobObject(raw(&job), STOP_CODE) };
        if ok == 0 {
            return Err(ProcessError::Track(io::Error::last_os_error()));
        }
        Ok(())
    }
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle()
}

fn own(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    #[allow(unsafe_code)]
    // SAFETY: callers supply a newly returned, valid, uniquely owned Win32 handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn track_error(stage: &str, error: io::Error) -> ProcessError {
    ProcessError::Track(io::Error::new(error.kind(), format!("{stage}: {error}")))
}

fn new_job() -> io::Result<(Arc<OwnedHandle>, Arc<OwnedHandle>, Arc<OwnedHandle>)> {
    #[allow(unsafe_code)]
    // SAFETY: null security/name creates a private, non-inherited manual-reset event.
    let cancel_event = Arc::new(own(unsafe { CreateEventW(null(), 1, 0, null()) })?);
    #[allow(unsafe_code)]
    // SAFETY: null security/name pointers create a private, non-inherited unnamed job.
    let job = Arc::new(own(unsafe { CreateJobObjectW(null(), null()) })?);
    // Default limits deliberately omit JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE.
    #[allow(unsafe_code)]
    // SAFETY: INVALID_HANDLE_VALUE creates a new port; no file is associated here.
    let port = Arc::new(own(unsafe {
        CreateIoCompletionPort(INVALID_HANDLE_VALUE, null_mut(), 0, 1)
    })?);
    let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
        CompletionKey: JOB_KEY as *mut _,
        CompletionPort: raw(&port),
    };
    #[allow(unsafe_code)]
    // SAFETY: association is the structure required by this information class,
    // with its exact size. Both handles live beyond the association.
    let ok = unsafe {
        SetInformationJobObject(
            raw(&job),
            JobObjectAssociateCompletionPortInformation,
            (&association as *const JOBOBJECT_ASSOCIATE_COMPLETION_PORT).cast(),
            u32::try_from(std::mem::size_of_val(&association)).expect("Win32 structure size"),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((job, port, cancel_event))
}

fn assign(job: &OwnedHandle, process: &OwnedHandle) -> io::Result<()> {
    let mut already_assigned = 0;
    #[allow(unsafe_code)]
    // SAFETY: both handles are owned and the BOOL output is live writable storage.
    if unsafe { IsProcessInJob(raw(process), raw(job), &mut already_assigned) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if already_assigned != 0 {
        return Ok(());
    }
    #[allow(unsafe_code)]
    // SAFETY: both handles are owned; process was opened with job-assignment rights.
    if unsafe { AssignProcessToJobObject(raw(job), raw(process)) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn creation_time(process: &OwnedHandle) -> io::Result<u64> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    #[allow(unsafe_code)]
    // SAFETY: process supports query and each output points to a distinct live FILETIME.
    if unsafe {
        GetProcessTimes(
            raw(process),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

fn active_processes(job: &OwnedHandle) -> io::Result<u32> {
    let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    #[allow(unsafe_code)]
    // SAFETY: exact-size writable accounting structure for the requested class.
    if unsafe {
        QueryInformationJobObject(
            raw(job),
            JobObjectBasicAccountingInformation,
            (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
            u32::try_from(std::mem::size_of_val(&info)).expect("Win32 structure size"),
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(info.ActiveProcesses)
}

fn process_parents() -> io::Result<std::collections::BTreeMap<u32, u32>> {
    use std::collections::BTreeMap;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    #[allow(unsafe_code)]
    // SAFETY: snapshot has no pointer arguments and returns an owned handle.
    let snapshot = own(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })?;
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>())
            .expect("Win32 structure size"),
        ..Default::default()
    };
    let mut descendants = BTreeMap::new();
    #[allow(unsafe_code)]
    // SAFETY: entry's dwSize and buffer match the W API; snapshot is owned.
    let mut found = unsafe { Process32FirstW(raw(&snapshot), &mut entry) };
    while found != 0 {
        // Toolhelp can retain an exited process while another handle pins its
        // object. Zero execution threads confirms it cannot run game code;
        // treating mere PID presence as liveness would misclassify that stub.
        if entry.cntThreads != 0 {
            descendants.insert(entry.th32ProcessID, entry.th32ParentProcessID);
        }
        #[allow(unsafe_code)]
        // SAFETY: the same correctly initialized snapshot buffer remains live.
        let next = unsafe { Process32NextW(raw(&snapshot), &mut entry) };
        found = next;
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
        return Err(error);
    }
    Ok(descendants)
}

fn adopt_children(job: &OwnedHandle, leader: ProcessIdentity) -> io::Result<()> {
    use std::collections::{BTreeMap, BTreeSet};
    let descendants = process_parents()?;
    let mut parents = BTreeMap::from([(leader.pid, leader.start_time)]);
    let mut seen = BTreeSet::from([leader.pid]);
    // Keep verified parents pinned through the scan to prevent their PID reuse.
    let mut pinned = Vec::new();
    loop {
        let children: Vec<_> = descendants
            .iter()
            .filter(|(pid, parent)| !seen.contains(pid) && parents.contains_key(parent))
            .map(|(&pid, &parent)| (pid, parent))
            .collect();
        if children.is_empty() {
            break;
        }
        for (pid, parent) in children {
            seen.insert(pid);
            #[allow(unsafe_code)]
            // SAFETY: OpenProcess uses a snapshot PID; time and parent are verified below.
            let handle = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_QUOTA | PROCESS_TERMINATE,
                    0,
                    pid,
                )
            };
            if handle.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                    continue;
                }
                return Err(error);
            }
            let process = own(handle)?;
            let started = creation_time(&process)?;
            // A PID could have been recycled between snapshot and OpenProcess.
            // Recheck its parent while the child and its verified parent are pinned.
            if parents
                .get(&parent)
                .is_some_and(|&parent_start| started >= parent_start)
                && process_parents()?.get(&pid) == Some(&parent)
            {
                assign(job, &process)?;
                parents.insert(pid, started);
                pinned.push(process);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::process::tests::{game, wait_for_file};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn zero_job_accounting_still_waits_for_process_exit_and_can_cancel() {
        let (dir, launch) = game("wait");
        let mut running = Game::spawn(&launch).unwrap();
        wait_for_file(&dir.path().join("ready"));
        let killer = running.killer();
        let cancel = running.canceller();
        let process = running.process.clone();
        let entered = running.final_wait_entered.clone();
        // Model the ordering boundary deterministically: job accounting is
        // already zero but the owned leader handle has not been signaled.
        let (empty_job, empty_port, _) = new_job().unwrap();
        running.job = empty_job;
        running.port = empty_port;
        let (tx, result) = mpsc::channel();
        thread::spawn(move || tx.send(running.wait()).unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !entered.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "final process wait not entered");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(result.recv_timeout(Duration::from_millis(50)).is_err());
        cancel.cancel();
        assert_eq!(
            result
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap(),
            None
        );
        #[allow(unsafe_code)]
        // SAFETY: the owned synchronization handle remains valid; zero never blocks.
        let state = unsafe { WaitForSingleObject(raw(&process), 0) };
        assert_eq!(state, windows_sys::Win32::Foundation::WAIT_TIMEOUT);
        killer.terminate(true).unwrap();
    }
}
