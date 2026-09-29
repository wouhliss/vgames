use std::fs;

use serde_json::json;
use tempfile::TempDir;
use vgames_core::manifest::Manifest;

use super::*;

/// Execution-relevant fields only; `resolve` does not re-run validation.
pub(crate) fn manifest(multiplayer: bool) -> Manifest {
    let mut value = json!({
        "format": "vgames.manifest/1",
        "server_id": "01920000-0000-7000-8000-000000000000",
        "package_id": "0192a6f0-1c2d-7e3f-8a9b-0c1d2e3f4a5b",
        "version_id": "0192a6f1-aaaa-7bbb-8ccc-dddddddddddd",
        "sequence": 1,
        "version_label": "1.0",
        "platform": "linux-x86_64",
        "created_at": "2026-09-24T10:00:00Z",
        "chunk_size": 4194304,
        "totals": { "files": 0, "bytes": 0, "chunks": 0, "packs": 0 },
        "packs": [], "chunks": [], "files": [],
        "launch": {
            "default": "play",
            "targets": [
                { "id": "play", "label": "Play", "executable": "Game/bin/game",
                  "args": ["-windowed"], "env": { "GAME_LANG": "en" } },
                { "id": "tools", "label": "Tools", "executable": "Game/tools",
                  "args": [], "working_dir": "Game" }
            ]
        }
    });
    if multiplayer {
        value["multiplayer"] =
            json!({ "join": { "target": "play", "args": ["+connect", "{join_secret}"] } });
    }
    serde_json::from_value(value).unwrap()
}

pub(crate) fn install() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("Game/bin")).unwrap();
    fs::write(dir.path().join("Game/bin/game"), b"exe").unwrap();
    fs::write(dir.path().join("Game/tools"), b"exe").unwrap();
    dir
}

#[test]
fn default_target_runs_in_the_executable_directory() {
    let dir = install();
    let root = fs::canonicalize(dir.path()).unwrap();
    let target = resolve(dir.path(), &manifest(false), &TargetChoice::Default).unwrap();
    assert_eq!(target.id, "play");
    assert_eq!(target.executable, root.join("Game/bin/game"));
    assert_eq!(target.working_dir, root.join("Game/bin"));
    assert_eq!(target.args, ["-windowed"]);
    assert_eq!(target.env.get("GAME_LANG").map(String::as_str), Some("en"));
}

#[test]
fn named_target_uses_its_working_dir() {
    let dir = install();
    let root = fs::canonicalize(dir.path()).unwrap();
    let target = resolve(
        dir.path(),
        &manifest(true),
        &TargetChoice::Target("tools".into()),
    )
    .unwrap();
    assert_eq!(target.working_dir, root.join("Game"));
    assert!(target.args.is_empty());
    let unknown = resolve(
        dir.path(),
        &manifest(true),
        &TargetChoice::Target("nope".into()),
    );
    assert!(matches!(unknown, Err(TargetError::UnknownTarget(id)) if id == "nope"));
}

#[test]
fn join_secret_is_one_whole_argument() {
    let dir = install();
    let choice = TargetChoice::for_invite(Some("[2001:db8::1]:27015"));
    let target = resolve(dir.path(), &manifest(true), &choice).unwrap();
    assert_eq!(
        target.args,
        ["-windowed", "+connect", "[2001:db8::1]:27015"]
    );
}

#[test]
fn invalid_or_absent_secrets_launch_normally() {
    let long = "a".repeat(257);
    for raw in [
        None,
        Some(""),
        Some("a b"),
        Some("x;rm -rf"),
        Some("é"),
        Some("-a\n"),
        Some(long.as_str()),
    ] {
        assert_eq!(
            TargetChoice::for_invite(raw),
            TargetChoice::Default,
            "{raw:?}"
        );
    }
    assert!(JoinSecret::parse(&"a".repeat(256)).is_some());
    // A valid secret for a package without `multiplayer.join` is a normal launch too.
    let dir = install();
    let choice = TargetChoice::for_invite(Some("lobby-42"));
    let target = resolve(dir.path(), &manifest(false), &choice).unwrap();
    assert_eq!(target.args, ["-windowed"]);
    assert_eq!(format!("{choice:?}"), "Join(JoinSecret(..))");
}

#[test]
fn missing_executable_is_refused() {
    let dir = install();
    fs::remove_file(dir.path().join("Game/bin/game")).unwrap();
    let result = resolve(dir.path(), &manifest(false), &TargetChoice::Default);
    assert!(matches!(
        result,
        Err(TargetError::OutsideInstall {
            what: "executable",
            ..
        })
    ));
}

#[test]
fn executable_that_is_a_directory_is_refused() {
    let dir = install();
    fs::remove_file(dir.path().join("Game/tools")).unwrap();
    fs::create_dir(dir.path().join("Game/tools")).unwrap();
    let result = resolve(
        dir.path(),
        &manifest(false),
        &TargetChoice::Target("tools".into()),
    );
    assert!(matches!(
        result,
        Err(TargetError::OutsideInstall {
            what: "executable",
            ..
        })
    ));
}

#[cfg(unix)]
#[test]
fn symlinks_out_of_the_install_are_refused() {
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("evil"), b"exe").unwrap();

    let dir = install();
    fs::remove_file(dir.path().join("Game/bin/game")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("evil"),
        dir.path().join("Game/bin/game"),
    )
    .unwrap();
    let result = resolve(dir.path(), &manifest(false), &TargetChoice::Default);
    assert!(matches!(
        result,
        Err(TargetError::OutsideInstall {
            what: "executable",
            ..
        })
    ));

    // A linked directory component escapes too.
    let dir = install();
    fs::remove_dir_all(dir.path().join("Game/bin")).unwrap();
    fs::create_dir(outside.path().join("bin")).unwrap();
    fs::write(outside.path().join("bin/game"), b"exe").unwrap();
    std::os::unix::fs::symlink(outside.path().join("bin"), dir.path().join("Game/bin")).unwrap();
    let result = resolve(dir.path(), &manifest(false), &TargetChoice::Default);
    assert!(matches!(
        result,
        Err(TargetError::OutsideInstall {
            what: "executable",
            ..
        })
    ));

    // A working directory linked outside is refused even with a valid executable.
    let dir = install();
    fs::remove_file(dir.path().join("Game/tools")).unwrap();
    fs::write(dir.path().join("tools-real"), b"exe").unwrap();
    let mut m = manifest(false);
    let target = &mut m.launch.as_mut().unwrap().targets[1];
    target.executable = "tools-real".into();
    target.working_dir = Some("Linked".into());
    std::os::unix::fs::symlink(outside.path(), dir.path().join("Linked")).unwrap();
    let result = resolve(dir.path(), &m, &TargetChoice::Target("tools".into()));
    assert!(matches!(
        result,
        Err(TargetError::OutsideInstall {
            what: "working directory",
            ..
        })
    ));
}

#[test]
fn package_without_targets_cannot_launch() {
    let dir = install();
    let mut m = manifest(false);
    m.launch = None;
    let result = resolve(dir.path(), &m, &TargetChoice::Default);
    assert!(matches!(result, Err(TargetError::NoTargets)));
}
