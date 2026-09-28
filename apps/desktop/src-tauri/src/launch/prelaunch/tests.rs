use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use tempfile::TempDir;
use vgames_pack::manifest::{Execution, Launch, LaunchTarget};
use vgames_pack::{Compression, PACK_SIZE};
use vgames_transfer::install::{InstallRecord, write_record};
use vgames_transfer::testkit::{FileSpec, Identity, TestPackage, trust_state_with, write_tree};

use super::*;

struct Installed {
    root: TempDir,
    package: TestPackage,
}

fn installed(state: InstallState) -> Installed {
    installed_sized(state, 64 * 1024)
}

fn installed_sized(state: InstallState, exe_size: u64) -> Installed {
    let files = [
        FileSpec::random("bin/game", exe_size, 1),
        FileSpec::random("data/level.pak", 1000, 2),
    ];
    let execution = Execution {
        launch: Some(Launch {
            default: "play".into(),
            targets: vec![LaunchTarget {
                id: "play".into(),
                label: "Play".into(),
                executable: "bin/game".into(),
                args: vec!["-fast".into()],
                working_dir: None,
                env: BTreeMap::new(),
            }],
        }),
        ..Execution::default()
    };
    let package = TestPackage::build_executable(
        &files,
        &[],
        Compression::None,
        PACK_SIZE,
        &Identity::default(),
        &execution,
    );
    let root = tempfile::tempdir().unwrap();
    write_tree(root.path(), &files, &[]);
    let meta = root.path().join(META_DIR);
    fs::create_dir_all(&meta).unwrap();
    fs::write(meta.join(MANIFEST_FILE), &package.manifest).unwrap();
    fs::write(meta.join(SIGNATURE_FILE), package.envelope.to_bytes()).unwrap();
    let release = package.release();
    write_record(
        root.path(),
        &InstallRecord::for_manifest(&release.verified, state),
    )
    .unwrap();
    Installed { root, package }
}

impl Installed {
    fn check(&self, cache: &Prelaunch) -> Result<Checked, PrelaunchError> {
        cache.check(
            self.root.path(),
            &self.package.trust,
            &self.package.expected,
            &TargetChoice::Default,
        )
    }
}

#[test]
fn a_verified_install_passes_and_repeat_checks_skip_hashing() {
    let install = installed(InstallState::Installed);
    let cache = Prelaunch::default();
    let checked = install.check(&cache).unwrap();
    assert_eq!(checked.target.args, ["-fast"]);
    assert!(checked.target.executable.ends_with("bin/game"));
    install.check(&cache).unwrap();
    assert_eq!(cache.hashed.load(Ordering::Relaxed), 1);

    cache.forget(install.root.path());
    install.check(&cache).unwrap();
    assert_eq!(cache.hashed.load(Ordering::Relaxed), 2);
}

#[test]
fn a_modified_executable_blocks_the_launch_even_after_a_cached_pass() {
    let install = installed(InstallState::Installed);
    let cache = Prelaunch::default();
    install.check(&cache).unwrap();

    // Same size, different bytes.
    let exe = install.root.path().join("bin/game");
    let mut bytes = fs::read(&exe).unwrap();
    bytes[100] ^= 1;
    fs::write(&exe, &bytes).unwrap();
    assert!(matches!(
        install.check(&cache),
        Err(PrelaunchError::ExecutableModified)
    ));
    // And it stays blocked.
    assert!(matches!(
        install.check(&cache),
        Err(PrelaunchError::ExecutableModified)
    ));
}

#[test]
fn incomplete_installs_do_not_launch() {
    let install = installed(InstallState::Installing);
    let cache = Prelaunch::default();
    assert!(matches!(
        install.check(&cache),
        Err(PrelaunchError::NotInstalled)
    ));

    let install = installed(InstallState::Installed);
    fs::remove_file(install.root.path().join(META_DIR).join(RECORD_FILE)).unwrap();
    assert!(matches!(
        install.check(&cache),
        Err(PrelaunchError::NotInstalled)
    ));
}

#[test]
fn a_revoked_key_asks_for_reverification_despite_the_cache() {
    let install = installed(InstallState::Installed);
    let cache = Prelaunch::default();
    install.check(&cache).unwrap();
    let revoked = trust_state_with(2, true);
    let result = cache.check(
        install.root.path(),
        &revoked,
        &install.package.expected,
        &TargetChoice::Default,
    );
    assert!(matches!(result, Err(PrelaunchError::Reverify)));
}

#[test]
fn tampered_metadata_fails_integrity() {
    let install = installed(InstallState::Installed);
    let cache = Prelaunch::default();
    install.check(&cache).unwrap();
    let manifest = install.root.path().join(META_DIR).join(MANIFEST_FILE);
    let mut bytes = fs::read(&manifest).unwrap();
    let at = bytes.iter().position(|&b| b == b'1').unwrap();
    bytes[at] = b'2';
    fs::write(&manifest, &bytes).unwrap();
    assert!(matches!(
        install.check(&cache),
        Err(PrelaunchError::Integrity(_))
    ));

    // A record that names another manifest.
    let install = installed(InstallState::Installed);
    let mut record = install::read_record(install.root.path()).unwrap().unwrap();
    record.manifest_blake3 = "00".repeat(32);
    write_record(install.root.path(), &record).unwrap();
    assert!(matches!(
        install.check(&cache),
        Err(PrelaunchError::Integrity(_))
    ));
}

#[test]
fn another_release_is_not_accepted() {
    let install = installed(InstallState::Installed);
    let cache = Prelaunch::default();
    let mut expected = install.package.expected.clone();
    expected.sequence += 1;
    let result = cache.check(
        install.root.path(),
        &install.package.trust,
        &expected,
        &TargetChoice::Default,
    );
    assert!(matches!(result, Err(PrelaunchError::Integrity(_))));
}

/// 02 §11 budgets: < 300 ms for a typical package, < 20 ms when cached.
/// `cargo test -p vgames-desktop --release --lib prelaunch_budget -- --ignored --nocapture`
#[test]
#[ignore = "benchmark"]
fn prelaunch_budget() {
    let install = installed_sized(InstallState::Installed, 200 * 1024 * 1024);
    let cache = Prelaunch::default();
    let started = std::time::Instant::now();
    install.check(&cache).unwrap();
    let cold = started.elapsed();
    let started = std::time::Instant::now();
    for _ in 0..100 {
        install.check(&cache).unwrap();
    }
    let warm = started.elapsed() / 100;
    println!("pre-launch check, 200 MiB executable: first {cold:?}, cached {warm:?}");
    assert!(cold < std::time::Duration::from_millis(300), "{cold:?}");
    assert!(warm < std::time::Duration::from_millis(20), "{warm:?}");
}
