use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::*;
use crate::launch::{LaunchPlan, PreparedLaunch, ResolvedTarget};

/// A "game" that is a shell script; children started with `&` stay in its group.
fn game(script: &str) -> (TempDir, PreparedLaunch) {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("game");
    std::fs::write(&exe, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let target = ResolvedTarget {
        id: "play".into(),
        executable: exe,
        args: Vec::new(),
        working_dir: dir.path().to_owned(),
        env: BTreeMap::new(),
    };
    let launch = LaunchPlan::Native
        .prepare(target, std::env::vars_os())
        .unwrap();
    (dir, launch)
}

fn wait_in_thread(game: RunningGame) -> mpsc::Receiver<Option<GameExit>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(game.wait().unwrap()).unwrap());
    rx
}

fn wait_for_file(path: &Path) {
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
    let (_dir, launch) = game("sleep 0.6 &\nexit 3");
    let started = Instant::now();
    let exit = RunningGame::spawn(&launch).unwrap().wait().unwrap();
    assert_eq!(exit, Some(GameExit::Code(3)));
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn escaped_children_are_not_tracked() {
    // `setsid` moves the child into its own group: the game counts as stopped.
    let (_dir, launch) = game(
        "setsid sh -c 'touch left; exec sleep 5' &\nuntil [ -e left ]; do sleep 0.01; done\nexit 0",
    );
    let started = Instant::now();
    let exit = RunningGame::spawn(&launch).unwrap().wait().unwrap();
    assert_eq!(exit, Some(GameExit::Code(0)));
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[test]
fn reattach_checks_the_start_time_and_tracks_the_tree() {
    let (dir, launch) = game("sleep 30 &\ntouch ready\nwait");
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
    assert_eq!(
        original.recv_timeout(timeout).unwrap(),
        Some(GameExit::Signal(libc::SIGTERM))
    );
    assert_eq!(
        reattached.recv_timeout(timeout).unwrap(),
        Some(GameExit::Unknown)
    );
    // The group is gone: terminating again must not signal anyone.
    killer.terminate(true).unwrap();
    assert!(RunningGame::reattach(identity).unwrap().is_none());
}

#[test]
fn reattach_finds_the_group_after_the_leader_exited() {
    let (dir, launch) = game("sleep 30 &\ntouch ready\nexit 0");
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
    let (_dir, launch) = game("exec sleep 30");
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
    let (dir, launch) = game("exit 0");
    std::fs::remove_file(dir.path().join("game")).unwrap();
    assert!(matches!(
        RunningGame::spawn(&launch),
        Err(ProcessError::Spawn(_))
    ));
}
