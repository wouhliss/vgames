use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::*;
use crate::launch::PreparedLaunch;

pub(super) fn game(mode: &str) -> (TempDir, PreparedLaunch) {
    let dir = tempfile::tempdir().unwrap();
    let launch = crate::launch::testgame::launch(dir.path(), mode);
    (dir, launch)
}

fn wait_in_thread(game: RunningGame) -> mpsc::Receiver<Option<GameExit>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(game.wait().unwrap()).unwrap());
    rx
}

pub(super) fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn waits_for_children_after_the_launched_process_exits() {
    let (_dir, launch) = game("tree");
    let started = Instant::now();
    let exit = RunningGame::spawn(&launch).unwrap().wait().unwrap();
    assert_eq!(exit, Some(GameExit::Code(3)));
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn escaped_children_are_not_tracked() {
    // `setsid` moves the child into its own group: the game counts as stopped.
    let (_dir, launch) = game("escape");
    let started = Instant::now();
    let exit = RunningGame::spawn(&launch).unwrap().wait().unwrap();
    assert_eq!(exit, Some(GameExit::Code(0)));
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[test]
fn reattach_checks_the_start_time_and_tracks_the_tree() {
    let (dir, launch) = game("tree-wait");
    let game = RunningGame::spawn(&launch).unwrap();
    let identity = game.identity();
    let killer = game.killer();
    wait_for_file(&dir.path().join("ready"));

    let reused = ProcessIdentity {
        start_time: identity.start_time + 1,
        ..identity
    };
    assert!(RunningGame::reattach(reused).unwrap().is_none());
    let again = RunningGame::reattach(identity)
        .unwrap()
        .expect("still running");
    assert_eq!(again.identity(), identity);

    let original = wait_in_thread(game);
    let reattached = wait_in_thread(again);
    killer.terminate(false).unwrap();
    let timeout = Duration::from_secs(10);
    #[cfg(not(windows))]
    let expected = GameExit::Signal(libc::SIGTERM);
    #[cfg(windows)]
    let expected = GameExit::Code(1);
    assert_eq!(original.recv_timeout(timeout).unwrap(), Some(expected));
    assert_eq!(
        reattached.recv_timeout(timeout).unwrap(),
        Some(GameExit::Unknown)
    );
    // The group is gone: terminating again must not signal anyone.
    killer.terminate(true).unwrap();
    assert!(RunningGame::reattach(identity).unwrap().is_none());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn reattach_finds_the_group_after_the_leader_exited() {
    let (dir, launch) = game("orphan");
    let game = RunningGame::spawn(&launch).unwrap();
    let identity = game.identity();
    wait_for_file(&dir.path().join("ready"));

    // The leader is unreaped (a zombie) or gone; its child still runs.
    let again = RunningGame::reattach(identity)
        .unwrap()
        .expect("child still running");
    let rx = wait_in_thread(again);
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    game.killer().terminate(true).unwrap();
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        Some(GameExit::Unknown)
    );
    assert_eq!(game.wait().unwrap(), Some(GameExit::Code(0)));
}

#[test]
fn cancelling_the_wait_leaves_the_game_running() {
    let (_dir, launch) = game("wait");
    let game = RunningGame::spawn(&launch).unwrap();
    let identity = game.identity();
    let canceller = game.canceller();
    let killer = game.killer();
    let rx = wait_in_thread(game);
    canceller.cancel();
    assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), None);

    let again = RunningGame::reattach(identity)
        .unwrap()
        .expect("game survived");
    killer.terminate(true).unwrap();
    assert_eq!(again.wait().unwrap(), Some(GameExit::Unknown));
}

#[test]
fn spawn_failure_is_reported() {
    let (dir, mut launch) = game("exit");
    launch.program = dir.path().join("missing-game");
    assert!(matches!(
        RunningGame::spawn(&launch),
        Err(ProcessError::Spawn(_))
    ));
}

#[test]
fn closing_all_launcher_handles_leaves_the_game_running() {
    let (dir, launch) = game("tree-wait");
    let game = RunningGame::spawn(&launch).unwrap();
    let identity = game.identity();
    wait_for_file(&dir.path().join("ready"));
    drop(game);
    let again = RunningGame::reattach(identity)
        .unwrap()
        .expect("game survived handle closure");
    again.killer().terminate(true).unwrap();
    assert_eq!(again.wait().unwrap(), Some(GameExit::Unknown));
}

#[cfg(target_os = "macos")]
#[test]
fn kqueue_wait_has_no_periodic_wakeups() {
    let (dir, launch) = game("wait");
    let game = RunningGame::spawn(&launch).unwrap();
    wait_for_file(&dir.path().join("ready"));
    let identity = game.identity();
    let iterations = game.inner.wait_counter();
    let canceller = game.canceller();
    let killer = game.killer();
    let result = wait_in_thread(game);
    let deadline = Instant::now() + Duration::from_secs(10);
    while iterations.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        assert!(Instant::now() < deadline, "tracker never entered kevent");
        thread::sleep(Duration::from_millis(5));
    }
    let before = iterations.load(std::sync::atomic::Ordering::Relaxed);
    thread::sleep(Duration::from_secs(1));
    assert_eq!(
        iterations.load(std::sync::atomic::Ordering::Relaxed),
        before
    );
    assert!(result.try_recv().is_err());
    canceller.cancel();
    assert_eq!(result.recv_timeout(Duration::from_secs(10)).unwrap(), None);
    let again = RunningGame::reattach(identity)
        .unwrap()
        .expect("cancel left game alive");
    killer.terminate(true).unwrap();
    assert_eq!(again.wait().unwrap(), Some(GameExit::Unknown));
}

#[cfg(windows)]
#[test]
fn reattach_cannot_recover_an_orphan_tree_without_the_original_job_handle() {
    let (dir, launch) = game("orphan");
    let game = RunningGame::spawn(&launch).unwrap();
    wait_for_file(&dir.path().join("ready"));
    // Wait for the leader handle to signal, without waiting for its live child.
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    #[allow(unsafe_code)]
    // SAFETY: OpenProcess takes the launcher's fresh identity and returns a checked handle.
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, game.identity().pid) };
    assert!(!handle.is_null());
    #[allow(unsafe_code)]
    // SAFETY: this successful OpenProcess handle is uniquely owned here.
    let process = unsafe { OwnedHandle::from_raw_handle(handle) };
    #[allow(unsafe_code)]
    // SAFETY: the process handle supports synchronization and the wait is bounded.
    let result = unsafe { WaitForSingleObject(process.as_raw_handle(), 10_000) };
    assert_eq!(result, 0);
    assert!(RunningGame::reattach(game.identity()).unwrap().is_none());
    game.killer().terminate(true).unwrap();
    assert_eq!(game.wait().unwrap(), Some(GameExit::Code(0)));
}
