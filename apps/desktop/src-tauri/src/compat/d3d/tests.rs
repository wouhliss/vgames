use std::collections::BTreeMap;

use vgames_pack::manifest::{Execution, Launch, LaunchTarget};
use vgames_pack::{Compression, PACK_SIZE};
use vgames_transfer::testkit::{FileSpec, Identity, TestPackage};

use super::*;
use crate::compat::pe::tests::pe;

fn manifest(platform: Platform) -> Manifest {
    let target = |id: &str, exe: &str| LaunchTarget {
        id: id.into(),
        label: id.into(),
        executable: exe.into(),
        args: Vec::new(),
        working_dir: None,
        env: BTreeMap::new(),
    };
    let execution = Execution {
        launch: Some(Launch {
            default: "play".into(),
            targets: vec![
                target("play", "Game/Game.exe"),
                target("safe", "Game/Game.exe"),
                target("tool", "Tools/Editor.exe"),
            ],
        }),
        ..Execution::default()
    };
    let files = [
        FileSpec::random("Game/Game.exe", 100, 1),
        FileSpec::random("Tools/Editor.exe", 100, 2),
    ];
    let package = TestPackage::build_executable(
        &files,
        &[],
        Compression::None,
        PACK_SIZE,
        &Identity {
            platform,
            ..Identity::default()
        },
        &execution,
    );
    package.release().manifest().clone()
}

const PACKAGE: PackageRef = PackageRef {
    server_id: Uuid::from_u128(1),
    package_id: Uuid::from_u128(2),
};

#[test]
fn only_windows_builds_on_a_mac_fetch_their_executables_first() {
    let db = Db::open_in_memory().unwrap();
    let mac = D3dCheck::new(db.clone(), Some(Platform::MacosAarch64));
    assert_eq!(
        mac.priority_files(PACKAGE, &manifest(Platform::WindowsX86_64)),
        ["Game/Game.exe", "Tools/Editor.exe"]
    );
    assert!(
        mac.priority_files(PACKAGE, &manifest(Platform::MacosAarch64))
            .is_empty()
    );
    for host in [Platform::LinuxX86_64, Platform::WindowsX86_64] {
        let other = D3dCheck::new(db.clone(), Some(host));
        assert!(
            other
                .priority_files(PACKAGE, &manifest(Platform::WindowsX86_64))
                .is_empty(),
            "{host:?}"
        );
    }
}

#[tokio::test]
async fn a_d3d12_executable_is_refused_and_the_scan_is_recorded() {
    let db = Db::open_in_memory().unwrap();
    let check = D3dCheck::new(db.clone(), Some(Platform::MacosAarch64));
    let m = manifest(Platform::WindowsX86_64);
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("Game.exe");
    let editor = dir.path().join("Editor.exe");
    std::fs::write(&game, pe(&["kernel32.dll"], &["d3d12.dll"], false)).unwrap();
    std::fs::write(&editor, pe(&["d3d11.dll"], &[], false)).unwrap();

    check.priority_files(PACKAGE, &m);
    let refused = check.check(PACKAGE, &[game.clone(), editor.clone()]).await;
    assert_eq!(refused, Err(CompatBlocker::D3d12UnsupportedOnMac));
    let stored = recorded(&db, PACKAGE, m.version_id).await.unwrap().unwrap();
    assert!(stored.d3d12 && stored.d3d11 && !stored.d3d9);

    std::fs::write(&game, pe(&["d3d11.dll"], &[], true)).unwrap();
    check.priority_files(PACKAGE, &m);
    assert_eq!(check.check(PACKAGE, &[game.clone(), editor]).await, Ok(()));
    let stored = recorded(&db, PACKAGE, m.version_id).await.unwrap().unwrap();
    assert!(!stored.d3d12 && stored.d3d11);
}

#[tokio::test]
async fn an_unreadable_executable_does_not_block_the_install() {
    let db = Db::open_in_memory().unwrap();
    let check = D3dCheck::new(db.clone(), Some(Platform::MacosX86_64));
    let dir = tempfile::tempdir().unwrap();
    let odd = dir.path().join("Game.exe");
    std::fs::write(&odd, b"not a PE").unwrap();
    check.priority_files(PACKAGE, &manifest(Platform::WindowsX86_64));
    assert_eq!(check.check(PACKAGE, &[odd]).await, Ok(()));
}
