//! Portable game fixture: re-execute just one test in the current test binary.
//! No shell, external executable, or platform-specific script is needed.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::{LaunchPlan, PreparedLaunch, ResolvedTarget};

pub const TEST_NAME: &str = "launch::testgame::dummy_game";

pub fn launch(root: &Path, mode: &str) -> PreparedLaunch {
    let target = ResolvedTarget {
        id: "play".into(),
        executable: std::env::current_exe().unwrap(),
        args: vec!["--exact".into(), TEST_NAME.into(), "--nocapture".into()],
        working_dir: root.to_owned(),
        env: BTreeMap::from([("VGAMES_TEST_MODE".into(), mode.into())]),
    };
    LaunchPlan::Native
        .prepare(target, std::env::vars_os())
        .unwrap()
}

fn child(mode: &str) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env("VGAMES_TEST_MODE", mode)
        .spawn()
        .unwrap()
}

#[test]
fn dummy_game() {
    let Ok(mode) = std::env::var("VGAMES_TEST_MODE") else {
        return;
    };
    match mode.as_str() {
        "exit" => {
            let code = std::env::var("VGAMES_TEST_EXIT").unwrap_or_else(|_| "7".into());
            std::process::exit(code.parse().unwrap());
        }
        "short-child" => std::thread::sleep(Duration::from_millis(600)),
        "wait" => {
            std::fs::write("ready", b"ready").unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
        "tree" | "tree-wait" => {
            let mut child = child(if mode == "tree" {
                "short-child"
            } else {
                "wait"
            });
            std::fs::write("parent-ready", child.id().to_string()).unwrap();
            if mode == "tree" {
                std::process::exit(3)
            }
            child.wait().unwrap();
        }
        "orphan" => {
            let child = child("wait");
            std::fs::write("parent-ready", child.id().to_string()).unwrap();
            std::process::exit(0);
        }
        #[cfg(target_os = "linux")]
        "escape" => {
            let _child = child("escaped-child");
            while !Path::new("left").exists() {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::process::exit(0);
        }
        #[cfg(target_os = "linux")]
        "escaped-child" => {
            #[allow(unsafe_code)]
            // SAFETY: setsid takes no arguments and only changes this fixture's session.
            let session = unsafe { libc::setsid() };
            assert!(session > 0);
            std::fs::write("left", b"left").unwrap();
            // This child must outlive the test's four-second tracking budget.
            std::thread::sleep(Duration::from_secs(5));
        }
        _ => panic!("unknown dummy game mode"),
    }
}
