use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::broadcast;
use vgames_pack::manifest::{Execution, Launch, LaunchTarget};
use vgames_pack::{Compression, PACK_SIZE};
use vgames_transfer::install::{
    InstallRecord, InstallState, MANIFEST_FILE, META_DIR, SIGNATURE_FILE, write_record,
};
use vgames_transfer::testkit::{FileSpec, Identity, TestPackage, trust_state_with, write_tree};

use super::*;
use crate::db::rusqlite;
use crate::events::{AppEvent, EventBus, GameStopped};

const GAME: &[u8] = b"#!/bin/sh\nexit 7\n";

struct FixedTrust(Option<Arc<TrustState>>);

impl TrustSource for FixedTrust {
    async fn trust(&self, _: Uuid) -> Result<Option<Arc<TrustState>>, DbError> {
        Ok(self.0.clone())
    }
}

struct Fixture {
    library: TempDir,
    package: TestPackage,
    db: Db,
    bus: EventBus,
}

impl Fixture {
    fn package_ref(&self) -> PackageRef {
        PackageRef {
            server_id: self.package.expected.server_id,
            package_id: self.package.expected.package_id,
        }
    }

    fn root(&self) -> std::path::PathBuf {
        self.library.path().join("game")
    }

    fn launcher(&self, trust: Option<Arc<TrustState>>) -> Launcher<FixedTrust> {
        let sessions = GameSessions::new(self.db.clone(), self.bus.clone());
        Launcher::new(self.db.clone(), Arc::new(FixedTrust(trust)), sessions)
    }

    fn trusted(&self) -> Launcher<FixedTrust> {
        self.launcher(Some(Arc::new(self.package.trust.clone())))
    }

    async fn set_row(&self, column: &'static str, value: String) {
        let package = self.package_ref();
        self.db
            .call(move |conn| {
                conn.execute(
                    &format!("UPDATE installs SET {column} = ?1 WHERE package_id = ?2"),
                    [value, package.package_id.to_string()],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }
}

async fn fixture() -> Fixture {
    let files = [
        FileSpec::script("bin/game", GAME),
        FileSpec::random("data/level.pak", 1000, 2),
    ];
    let execution = Execution {
        launch: Some(Launch {
            default: "play".into(),
            targets: vec![LaunchTarget {
                id: "play".into(),
                label: "Play".into(),
                executable: "bin/game".into(),
                args: Vec::new(),
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
    let library = tempfile::tempdir().unwrap();
    let root = library.path().join("game");
    write_tree(&root, &files, &[]);
    let meta = root.join(META_DIR);
    fs::create_dir_all(&meta).unwrap();
    fs::write(meta.join(MANIFEST_FILE), &package.manifest).unwrap();
    fs::write(meta.join(SIGNATURE_FILE), package.envelope.to_bytes()).unwrap();
    let release = package.release();
    write_record(
        &root,
        &InstallRecord::for_manifest(&release.verified, InstallState::Installed),
    )
    .unwrap();

    let db = Db::open_in_memory().unwrap();
    let expected = package.expected.clone();
    let library_path = library.path().to_string_lossy().into_owned();
    db.call(move |conn| {
        conn.execute(
            "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at)
             VALUES (?1, 'https://example.test', 'Test', zeroblob(32), 'VG1', 0)",
            [expected.server_id.to_string()],
        )?;
        conn.execute(
            "INSERT INTO libraries (id, path, label, created_at) VALUES ('lib', ?1, 'Games', 0)",
            [library_path],
        )?;
        conn.execute(
            "INSERT INTO installs (server_id, package_id, library_id, dir_name, version_id,
             sequence, platform, state)
             VALUES (?1, ?2, 'lib', 'game', ?3, ?4, ?5, 'installed')",
            rusqlite::params![
                expected.server_id.to_string(),
                expected.package_id.to_string(),
                expected.version_id.to_string(),
                i64::try_from(expected.sequence).unwrap(),
                expected.platform.as_str(),
            ],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    Fixture {
        library,
        package,
        db,
        bus: EventBus::new(),
    }
}

async fn stopped(events: &mut broadcast::Receiver<AppEvent>) -> GameStopped {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("event")
            .unwrap();
        if let AppEvent::GameStopped(stopped) = event {
            return stopped;
        }
    }
}

#[tokio::test]
async fn a_verified_install_launches_and_repeats_are_rate_limited() {
    let f = fixture().await;
    let mut events = f.bus.subscribe();
    let launcher = f.trusted();
    launcher
        .launch(f.package_ref(), TargetChoice::Default)
        .await
        .unwrap();
    assert_eq!(stopped(&mut events).await.exit.code, Some(7));
    assert_eq!(
        launcher
            .launch(f.package_ref(), TargetChoice::Default)
            .await,
        Err(LaunchError::RateLimited)
    );
}

#[tokio::test]
async fn a_modified_executable_is_blocked() {
    let f = fixture().await;
    fs::write(f.root().join("bin/game"), b"#!/bin/sh\nexit 8\n").unwrap();
    assert_eq!(
        f.trusted()
            .launch(f.package_ref(), TargetChoice::Default)
            .await,
        Err(LaunchError::Integrity {
            path: "bin/game".into()
        })
    );
}

#[tokio::test]
async fn a_revoked_key_or_missing_trust_blocks_the_launch() {
    let f = fixture().await;
    let revoked = f.launcher(Some(Arc::new(trust_state_with(2, true))));
    assert_eq!(
        revoked.launch(f.package_ref(), TargetChoice::Default).await,
        Err(LaunchError::KeyRevoked)
    );
    let unknown = f.launcher(None);
    assert!(matches!(
        unknown.launch(f.package_ref(), TargetChoice::Default).await,
        Err(LaunchError::Io { .. })
    ));
}

#[tokio::test]
async fn install_state_and_placement_are_checked_first() {
    let f = fixture().await;
    let other = PackageRef {
        package_id: Uuid::from_u128(5),
        ..f.package_ref()
    };
    assert_eq!(
        f.trusted().launch(other, TargetChoice::Default).await,
        Err(LaunchError::NotInstalled)
    );
    assert_eq!(
        f.trusted()
            .launch(f.package_ref(), TargetChoice::Target("nope".into()))
            .await,
        Err(LaunchError::TargetNotFound)
    );

    f.set_row("state", "updating".into()).await;
    assert_eq!(
        f.trusted()
            .launch(f.package_ref(), TargetChoice::Default)
            .await,
        Err(LaunchError::Busy {
            state: BusyState::Updating
        })
    );
    f.set_row("state", "broken".into()).await;
    assert_eq!(
        f.trusted()
            .launch(f.package_ref(), TargetChoice::Default)
            .await,
        Err(LaunchError::Incomplete)
    );
    f.set_row("state", "installed".into()).await;

    // A Windows build cannot run natively on Linux yet (Proton is A2-T16).
    f.set_row("platform", "windows-x86_64".into()).await;
    assert!(matches!(
        f.trusted()
            .launch(f.package_ref(), TargetChoice::Default)
            .await,
        Err(LaunchError::CompatUnavailable { .. })
    ));
    f.set_row("platform", "linux-x86_64".into()).await;

    // The library's drive is gone.
    let library_path = f.library.path().to_string_lossy().into_owned();
    f.db.call(|conn| {
        conn.execute(
            "UPDATE libraries SET path = '/nonexistent/vgames-library'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        f.trusted()
            .launch(f.package_ref(), TargetChoice::Default)
            .await,
        Err(LaunchError::LibraryOffline {
            library_path: "/nonexistent/vgames-library".into()
        })
    );
    assert_ne!(library_path, "/nonexistent/vgames-library");
}

#[test]
fn native_platforms() {
    use Platform::*;
    assert!(runs_natively(LinuxX86_64, LinuxX86_64));
    assert!(runs_natively(WindowsX86_64, WindowsAarch64));
    assert!(runs_natively(MacosX86_64, MacosAarch64));
    assert!(!runs_natively(WindowsX86_64, LinuxX86_64));
    assert!(!runs_natively(LinuxAarch64, LinuxX86_64));
    assert!(!runs_natively(MacosAarch64, MacosX86_64));
}
