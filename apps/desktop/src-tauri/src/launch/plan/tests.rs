use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use vgames_core::compat::Graphics;

use super::*;
use crate::launch::ResolvedTarget;

fn target() -> ResolvedTarget {
    ResolvedTarget {
        id: "play".into(),
        executable: PathBuf::from("/lib/Game/game.exe"),
        args: vec!["-dx11".into(), "has space; $(no shell)".into()],
        working_dir: PathBuf::from("/lib/Game"),
        env: BTreeMap::from([("GAME_LANG".into(), "en".into())]),
    }
}

fn host() -> Vec<(OsString, OsString)> {
    [
        ("PATH", "/usr/bin"),
        ("HOME", "/home/p"),
        ("VGAMES_TOKEN", "secret"),
        ("LD_PRELOAD", "/tmp/evil.so"),
        ("HTTPS_PROXY", "http://proxy"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect()
}

fn env<'a>(launch: &'a PreparedLaunch, key: &str) -> Option<&'a str> {
    launch.env.get(&fold_key(key)).and_then(|v| v.to_str())
}

fn proton() -> ProtonPlan {
    ProtonPlan {
        umu_run: "/rt/umu/umu-run".into(),
        proton_path: "/rt/proton".into(),
        prefix: "/data/prefixes/s/p".into(),
        umu_game_id: Some("umu-12345".into()),
        store: "none".into(),
        env: BTreeMap::from([
            ("PROTON_ENABLE_NVAPI".into(), "1".into()),
            ("GAME_LANG".into(), "fr".into()),
        ]),
        dll_overrides: "d3dcompiler_47=native,builtin".into(),
    }
}

#[test]
fn native_runs_the_executable_with_filtered_env() {
    let launch = LaunchPlan::Native.prepare(target(), host()).unwrap();
    assert_eq!(launch.program, PathBuf::from("/lib/Game/game.exe"));
    assert_eq!(launch.args, ["-dx11", "has space; $(no shell)"]);
    assert_eq!(launch.working_dir, PathBuf::from("/lib/Game"));
    assert_eq!(env(&launch, "PATH"), Some("/usr/bin"));
    assert_eq!(env(&launch, "GAME_LANG"), Some("en"));
    for dropped in ["VGAMES_TOKEN", "LD_PRELOAD", "HTTPS_PROXY"] {
        assert_eq!(env(&launch, dropped), None, "{dropped}");
    }
}

#[test]
fn proton_wraps_the_game_in_umu() {
    let launch = LaunchPlan::Proton(proton())
        .prepare(target(), host())
        .unwrap();
    assert_eq!(launch.program, PathBuf::from("/rt/umu/umu-run"));
    assert_eq!(
        launch.args,
        ["/lib/Game/game.exe", "-dx11", "has space; $(no shell)"]
    );
    assert_eq!(env(&launch, "WINEPREFIX"), Some("/data/prefixes/s/p"));
    assert_eq!(env(&launch, "PROTONPATH"), Some("/rt/proton"));
    assert_eq!(env(&launch, "GAMEID"), Some("umu-12345"));
    assert_eq!(env(&launch, "STORE"), Some("none"));
    assert_eq!(
        env(&launch, "WINEDLLOVERRIDES"),
        Some("d3dcompiler_47=native,builtin")
    );
    assert_eq!(env(&launch, "PROTON_ENABLE_NVAPI"), Some("1"));
    // The compat profile overrides the manifest.
    assert_eq!(env(&launch, "GAME_LANG"), Some("fr"));

    let mut plan = proton();
    plan.umu_game_id = None;
    plan.dll_overrides.clear();
    let launch = LaunchPlan::Proton(plan).prepare(target(), host()).unwrap();
    assert_eq!(env(&launch, "GAMEID"), Some("0"));
    assert_eq!(env(&launch, "WINEDLLOVERRIDES"), None);
}

#[test]
fn wine_wraps_the_game() {
    let plan = WinePlan {
        wine: "/rt/wine/bin/wine".into(),
        prefix: "/data/prefixes/s/p".into(),
        graphics: Graphics::Dxmt,
        env: BTreeMap::new(),
        dll_overrides: "d3d11,dxgi=n,b".into(),
    };
    let launch = LaunchPlan::Wine(plan).prepare(target(), host()).unwrap();
    assert_eq!(launch.program, PathBuf::from("/rt/wine/bin/wine"));
    assert_eq!(
        launch.args.first().map(OsString::as_os_str),
        Some("/lib/Game/game.exe".as_ref())
    );
    assert_eq!(env(&launch, "WINEDLLOVERRIDES"), Some("d3d11,dxgi=n,b"));
    assert_eq!(env(&launch, "PROTONPATH"), None);
}

#[test]
fn denied_or_runner_keys_are_refused() {
    let mut bad = target();
    bad.env.insert("LD_PRELOAD".into(), "x".into());
    assert!(
        matches!(LaunchPlan::Native.prepare(bad, host()), Err(LaunchError::EnvKey(k)) if k == "LD_PRELOAD")
    );

    let mut plan = proton();
    plan.env.insert("WINEPREFIX".into(), "/elsewhere".into());
    let result = LaunchPlan::Proton(plan).prepare(target(), host());
    assert!(matches!(result, Err(LaunchError::EnvKey(k)) if k == "WINEPREFIX"));
}

#[test]
fn injected_env_cannot_replace_runner_variables() {
    let mut launch = LaunchPlan::Proton(proton())
        .prepare(target(), host())
        .unwrap();
    launch
        .inject_env("VGAMES_OVERLAY_ENDPOINT", "127.0.0.1:4000")
        .unwrap();
    launch.inject_env("LD_PRELOAD", "/app/overlay.so").unwrap();
    assert_eq!(
        env(&launch, "VGAMES_OVERLAY_ENDPOINT"),
        Some("127.0.0.1:4000")
    );
    assert!(launch.inject_env("WINEPREFIX", "/x").is_err());
    assert!(launch.inject_env("bad key", "x").is_err());
    assert_eq!(env(&launch, "WINEPREFIX"), Some("/data/prefixes/s/p"));

    let mut native = LaunchPlan::Native.prepare(target(), host()).unwrap();
    native.inject_env("WINEPREFIX", "/x").unwrap();
}

#[test]
fn command_clears_the_inherited_environment() {
    let launch = LaunchPlan::Native.prepare(target(), host()).unwrap();
    let command = launch.command();
    assert_eq!(command.get_program(), "/lib/Game/game.exe");
    let envs: BTreeMap<_, _> = command.get_envs().collect();
    assert_eq!(envs.len(), launch.env.len());
    assert!(envs.values().all(Option::is_some));
    assert_eq!(
        command.get_current_dir(),
        Some(PathBuf::from("/lib/Game").as_path())
    );
}

#[cfg(unix)]
#[test]
fn spawned_game_sees_only_the_prepared_environment() {
    use std::os::unix::fs::PermissionsExt;

    let dir = crate::launch::target::tests::install();
    let script = dir.path().join("Game/bin/game");
    std::fs::write(&script, "#!/bin/sh\nprintf '%s|' \"$@\"\nenv | sort\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let manifest = crate::launch::target::tests::manifest(true);
    let choice = crate::launch::TargetChoice::for_invite(Some("lobby-7"));
    let target = crate::launch::target::resolve(dir.path(), &manifest, &choice).unwrap();

    let launch = LaunchPlan::Native.prepare(target, host()).unwrap();
    let output = launch.command().output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.starts_with("-windowed|+connect|lobby-7|"),
        "{stdout}"
    );
    assert!(stdout.contains("GAME_LANG=en"), "{stdout}");
    assert!(!stdout.contains("VGAMES_TOKEN"), "{stdout}");
    assert!(!stdout.contains("LD_PRELOAD"), "{stdout}");
}
