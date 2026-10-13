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

#[cfg(windows)]
const GAME_PATH: &str = "bin/game.exe";
#[cfg(not(windows))]
const GAME_PATH: &str = "bin/game";

fn game_bytes() -> &'static [u8] {
    static BINARY: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    BINARY.get_or_init(|| fs::read(std::env::current_exe().unwrap()).unwrap())
}

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
    fixture_with_controllers(None).await
}

async fn fixture_with_controllers(
    controllers: Option<vgames_pack::manifest::Controllers>,
) -> Fixture {
    let files = [
        FileSpec::script(GAME_PATH, game_bytes()),
        FileSpec::random("data/level.pak", 1000, 2),
    ];
    let execution = Execution {
        controllers,
        launch: Some(Launch {
            default: "play".into(),
            targets: vec![LaunchTarget {
                id: "play".into(),
                label: "Play".into(),
                executable: GAME_PATH.into(),
                args: vec![
                    "--exact".into(),
                    crate::launch::testgame::TEST_NAME.into(),
                    "--nocapture".into(),
                ],
                working_dir: None,
                env: BTreeMap::from([("VGAMES_TEST_MODE".into(), "exit".into())]),
            }],
        }),
        ..Execution::default()
    };
    let package = TestPackage::build_executable(
        &files,
        &[],
        Compression::None,
        PACK_SIZE,
        &Identity {
            platform: host_platform().unwrap(),
            ..Identity::default()
        },
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
async fn a_verified_install_launches_and_reports_its_exit() {
    let f = fixture().await;
    let mut events = f.bus.subscribe();
    let launcher = f.trusted();
    launcher
        .launch(f.package_ref(), TargetChoice::Default)
        .await
        .unwrap();
    assert_eq!(stopped(&mut events).await.exit.code, Some(7));
}

#[test]
fn launch_cooldown_rejects_repeats_until_the_exact_boundary() {
    let start = Instant::now();
    let mut last = None;
    assert_eq!(enforce_launch_interval(&mut last, start), Ok(()));
    for elapsed in [
        Duration::ZERO,
        Duration::from_millis(1),
        LAUNCH_INTERVAL - Duration::from_nanos(1),
    ] {
        assert_eq!(
            enforce_launch_interval(&mut last, start + elapsed),
            Err(LaunchError::RateLimited)
        );
        assert_eq!(
            last,
            Some(start),
            "rejected attempts must not extend the cooldown"
        );
    }
    assert_eq!(
        enforce_launch_interval(&mut last, start + LAUNCH_INTERVAL),
        Ok(())
    );
    assert_eq!(last, Some(start + LAUNCH_INTERVAL));
    assert_eq!(
        enforce_launch_interval(&mut last, start + LAUNCH_INTERVAL + Duration::from_nanos(1)),
        Err(LaunchError::RateLimited)
    );
}

struct ExitHook;

impl LaunchHooks for ExitHook {
    fn before_spawn<'a>(
        &'a self,
        _: &'a LaunchContext,
    ) -> HookFuture<'a, Result<HookEnv, LaunchVeto>> {
        Box::pin(async {
            Ok(HookEnv(vec![
                ("BAD KEY".into(), "x".into()),
                ("VGAMES_TEST_EXIT".into(), "9".into()),
            ]))
        })
    }

    fn aborted(&self, _: &LaunchContext) {}
}

#[tokio::test]
async fn launch_hooks_add_variables_and_invalid_keys_are_skipped() {
    let f = fixture().await;
    let mut events = f.bus.subscribe();
    let launcher = f.trusted();
    launcher
        .hooks()
        .register(HookStage::Overlay, Arc::new(ExitHook));
    launcher
        .launch(f.package_ref(), TargetChoice::Default)
        .await
        .unwrap();
    assert_eq!(stopped(&mut events).await.exit.code, Some(9));
}

#[tokio::test]
async fn a_modified_executable_is_blocked() {
    let f = fixture().await;
    fs::write(f.root().join(GAME_PATH), b"#!/bin/sh\nexit 8\n").unwrap();
    assert_eq!(
        f.trusted()
            .launch(f.package_ref(), TargetChoice::Default)
            .await,
        Err(LaunchError::Integrity {
            path: GAME_PATH.into()
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

    // A release for a different OS never falls back to a native spawn.
    let incompatible = if cfg!(windows) {
        "linux-x86_64"
    } else {
        "windows-x86_64"
    };
    f.set_row("platform", incompatible.into()).await;
    assert!(matches!(
        f.trusted()
            .launch(f.package_ref(), TargetChoice::Default)
            .await,
        Err(LaunchError::CompatUnavailable { .. })
    ));
    f.set_row("platform", host_platform().unwrap().as_str().into())
        .await;

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

struct SaveConflictHook;
impl LaunchHooks for SaveConflictHook {
    fn before_spawn<'a>(
        &'a self,
        _: &'a LaunchContext,
    ) -> HookFuture<'a, Result<HookEnv, LaunchVeto>> {
        Box::pin(async {
            Err(LaunchVeto::SaveConflict {
                conflict_id: "local-conflict".into(),
            })
        })
    }
    fn aborted(&self, _: &LaunchContext) {}
}

#[tokio::test]
async fn save_conflict_veto_reaches_the_command_without_spawning() {
    let f = fixture().await;
    let launcher = f.trusted();
    launcher
        .hooks()
        .register(HookStage::Saves, Arc::new(SaveConflictHook));
    assert_eq!(
        launcher
            .launch(f.package_ref(), TargetChoice::Default)
            .await,
        Err(LaunchError::SaveConflict {
            conflict_id: "local-conflict".into()
        })
    );
    assert!(!launcher.sessions.is_running(f.package_ref()));
    assert!(
        !f.root()
            .join(META_DIR)
            .join(super::super::session::SESSION_FILE)
            .exists()
    );
}

#[cfg(windows)]
struct FailsPreResume {
    observed: Arc<std::sync::atomic::AtomicBool>,
    failure: &'static str,
}

#[cfg(windows)]
impl LaunchHooks for FailsPreResume {
    fn before_spawn<'a>(
        &'a self,
        _: &'a LaunchContext,
    ) -> HookFuture<'a, Result<HookEnv, LaunchVeto>> {
        Box::pin(async { Ok(HookEnv::default()) })
    }

    fn pre_resume<'a>(
        &'a self,
        _: &'a LaunchContext,
        process: &'a super::super::process::SuspendedProcess,
    ) -> HookFuture<'a, Result<(), String>> {
        Box::pin(async move {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::Threading::{ResumeThread, SuspendThread};
            let thread = process.primary_thread_handle();
            #[allow(unsafe_code)]
            // SAFETY: the borrowed primary-thread handle is valid during this hook.
            // Increment then restore its suspension count to observe the original value.
            let before = unsafe { SuspendThread(thread.as_raw_handle()) };
            assert_ne!(before, u32::MAX);
            assert_eq!(before, 1, "main thread must already be suspended");
            if self.failure != "extra-suspension" {
                #[allow(unsafe_code)]
                // SAFETY: balance only the extra suspension added by this test hook.
                let restored = unsafe { ResumeThread(thread.as_raw_handle()) };
                assert_eq!(restored, 2);
            }
            self.observed
                .store(true, std::sync::atomic::Ordering::Release);
            match self.failure {
                "panic" => panic!("deliberate pre-resume panic"),
                "timeout" => std::future::pending().await,
                _ => Err("deliberate hook failure".into()),
            }
        })
    }

    fn aborted(&self, _: &LaunchContext) {}
}

#[cfg(windows)]
#[tokio::test]
async fn pre_resume_observes_suspension_and_a_failed_hook_still_runs_the_game() {
    for failure in ["error", "panic", "timeout", "extra-suspension"] {
        let f = fixture().await;
        let mut events = f.bus.subscribe();
        let launcher = f.trusted();
        let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        launcher.hooks().register(
            HookStage::Overlay,
            Arc::new(FailsPreResume {
                observed: observed.clone(),
                failure,
            }),
        );
        launcher
            .launch(f.package_ref(), TargetChoice::Default)
            .await
            .unwrap();
        assert_eq!(stopped(&mut events).await.exit.code, Some(7));
        assert!(observed.load(std::sync::atomic::Ordering::Acquire));
    }
}

struct ControllerContextHook(std::sync::Mutex<Option<LaunchContext>>);
impl LaunchHooks for ControllerContextHook {
    fn before_spawn<'a>(
        &'a self,
        ctx: &'a LaunchContext,
    ) -> HookFuture<'a, Result<HookEnv, LaunchVeto>> {
        *self.0.lock().unwrap() = Some(ctx.clone());
        Box::pin(async { Ok(HookEnv::default()) })
    }
    fn aborted(&self, _: &LaunchContext) {}
}

#[tokio::test]
async fn controller_hooks_receive_configuration_from_the_verified_signed_manifest() {
    use vgames_core::manifest::{ControllerKind, Controllers, EmulatedController};
    let config = Controllers {
        supported: vec![ControllerKind::Dualsense],
        emulate_as: Some(EmulatedController::Xbox360),
    };
    let f = fixture_with_controllers(Some(vgames_pack::manifest::Controllers {
        supported: vec!["dualsense".into()],
        emulate_as: Some("xbox360".into()),
    }))
    .await;
    let mut events = f.bus.subscribe();
    let launcher = f.trusted();
    let hook = Arc::new(ControllerContextHook(std::sync::Mutex::new(None)));
    assert!(
        launcher
            .hooks()
            .register(HookStage::Controllers, hook.clone())
    );
    launcher
        .launch(f.package_ref(), TargetChoice::Default)
        .await
        .unwrap();
    let ctx = hook.0.lock().unwrap().clone().unwrap();
    assert_eq!(ctx.package, f.package_ref());
    assert_eq!(ctx.controllers, Some(config));
    assert_ne!(ctx.session_id, Uuid::nil());
    stopped(&mut events).await;
}
