use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::broadcast;

use super::*;
use crate::launch::{LaunchPlan, ResolvedTarget};

const PACKAGE: PackageRef = PackageRef {
    server_id: Uuid::from_u128(0x0192_0000_0000_7000_8000_0000_0000_0001),
    package_id: Uuid::from_u128(0x0192_0000_0000_7000_8000_0000_0000_0002),
};

struct Fixture {
    install: TempDir,
    sessions: GameSessions,
    db: Db,
    events: broadcast::Receiver<AppEvent>,
}

async fn fixture() -> Fixture {
    let install = tempfile::tempdir().unwrap();
    std::fs::create_dir(install.path().join(META_DIR)).unwrap();
    let db = Db::open_in_memory().unwrap();
    // The install directory is `<library>/<dir_name>`.
    let library = install
        .path()
        .parent()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let dir_name = install
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    db.call(move |conn| {
        conn.execute(
            "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at)
             VALUES (?1, 'https://example.test', 'Test', zeroblob(32), 'VG1', 0)",
            [PACKAGE.server_id.to_string()],
        )?;
        conn.execute(
            "INSERT INTO libraries (id, path, label, created_at) VALUES ('lib', ?1, 'Games', 0)",
            [library],
        )?;
        conn.execute(
            "INSERT INTO installs (server_id, package_id, library_id, dir_name, version_id,
             sequence, platform, state, playtime_seconds)
             VALUES (?1, ?2, 'lib', ?3, 'v', 1, 'linux-x86_64', 'installed', 10)",
            [
                PACKAGE.server_id.to_string(),
                PACKAGE.package_id.to_string(),
                dir_name,
            ],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let bus = EventBus::new();
    let events = bus.subscribe();
    Fixture {
        install,
        sessions: GameSessions::new(db.clone(), bus),
        db,
        events,
    }
}

impl Fixture {
    fn launch(&self, script: &str) -> PreparedLaunch {
        let exe = self.install.path().join("game");
        std::fs::write(&exe, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let target = ResolvedTarget {
            id: "play".into(),
            executable: exe,
            args: Vec::new(),
            working_dir: self.install.path().to_owned(),
            env: BTreeMap::new(),
        };
        LaunchPlan::Native
            .prepare(target, std::env::vars_os())
            .unwrap()
    }

    fn root(&self) -> PathBuf {
        self.install.path().to_owned()
    }

    async fn next_event(&mut self) -> AppEvent {
        tokio::time::timeout(Duration::from_secs(10), self.events.recv())
            .await
            .expect("event")
            .unwrap()
    }

    async fn stopped(&mut self) -> GameStopped {
        loop {
            if let AppEvent::GameStopped(stopped) = self.next_event().await {
                return stopped;
            }
        }
    }
}

#[tokio::test]
async fn a_session_publishes_events_and_records_playtime() {
    let mut f = fixture().await;
    let pid = f
        .sessions
        .start(PACKAGE, f.root(), f.launch("sleep 0.3 &\nexit 4"))
        .await
        .unwrap();
    assert!(matches!(
        f.next_event().await,
        AppEvent::GameStarted(GameStarted { package, pid: p }) if package == PACKAGE && p == pid
    ));
    let stopped = f.stopped().await;
    assert_eq!(stopped.package, PACKAGE);
    assert_eq!(stopped.exit.code, Some(4));
    assert!(!stopped.exit.stopped_by_user);
    assert!(!record_path(&f.root()).exists());
    let (playtime, last_played) = db::installs::playtime(&f.db, PACKAGE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(playtime, 10 + i64::from(stopped.exit.session_seconds));
    assert!(last_played.is_some());
    assert!(!f.sessions.is_running(PACKAGE));
}

#[tokio::test]
async fn one_session_per_package_and_stop_ends_the_tree() {
    let mut f = fixture().await;
    f.sessions
        .start(PACKAGE, f.root(), f.launch("sleep 30 &\nwait"))
        .await
        .unwrap();
    assert!(record_path(&f.root()).exists());
    let again = f
        .sessions
        .start(PACKAGE, f.root(), f.launch("exit 0"))
        .await;
    assert!(matches!(again, Err(SessionError::AlreadyRunning)));

    f.sessions.stop(PACKAGE, false).unwrap();
    let stopped = f.stopped().await;
    assert!(stopped.exit.stopped_by_user);
    assert_eq!(stopped.exit.code, None);
    assert!(matches!(
        f.sessions.stop(PACKAGE, true),
        Err(SessionError::NotRunning)
    ));
}

#[tokio::test]
async fn a_failed_spawn_frees_the_slot() {
    let f = fixture().await;
    let launch = f.launch("exit 0");
    std::fs::remove_file(f.install.path().join("game")).unwrap();
    let result = f.sessions.start(PACKAGE, f.root(), launch).await;
    assert!(matches!(
        result,
        Err(SessionError::Process(ProcessError::Spawn(_)))
    ));
    assert!(!f.sessions.is_running(PACKAGE));
}

#[tokio::test]
async fn a_restarted_launcher_reattaches_and_credits_the_whole_session() {
    let f = fixture().await;
    f.sessions
        .start(PACKAGE, f.root(), f.launch("exec sleep 30"))
        .await
        .unwrap();
    // The launcher exits: waits stop, the game and its record stay.
    f.sessions.detach_all();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!f.sessions.is_running(PACKAGE));
    let mut record = read_record(&f.root()).unwrap();
    // Pretend the session began 100 s ago.
    record.started_at -= 100;
    write_record(&f.root(), &record).unwrap();

    let next = GameSessions::new(f.db.clone(), EventBus::new());
    let mut events = next.inner.bus.subscribe();
    assert!(next.reattach(PACKAGE, f.root()).await.unwrap());
    assert!(matches!(
        events.recv().await.unwrap(),
        AppEvent::GameStarted(_)
    ));
    next.stop(PACKAGE, true).unwrap();
    let stopped = loop {
        if let AppEvent::GameStopped(s) = events.recv().await.unwrap() {
            break s;
        }
    };
    assert!(stopped.exit.session_seconds >= 100);
    let (playtime, _) = db::installs::playtime(&f.db, PACKAGE)
        .await
        .unwrap()
        .unwrap();
    assert!(playtime >= 110);
    assert!(read_record(&f.root()).is_none());
}

#[tokio::test]
async fn stale_or_foreign_records_are_dropped() {
    let f = fixture().await;
    // Startup re-attach walks every registered install.
    let roots = db::installs::roots(&f.db).await.unwrap();
    assert_eq!(roots, [(PACKAGE, f.root())]);
    // Nothing recorded.
    assert!(!f.sessions.reattach(PACKAGE, f.root()).await.unwrap());

    // A pid that no longer runs this game.
    let record = SessionRecord {
        format: SESSION_FORMAT.into(),
        server_id: PACKAGE.server_id,
        package_id: PACKAGE.package_id,
        pid: std::process::id(),
        start_time: 1,
        started_at: 0,
    };
    write_record(&f.root(), &record).unwrap();
    assert!(!f.sessions.reattach(PACKAGE, f.root()).await.unwrap());
    assert!(!record_path(&f.root()).exists());

    // Another package's record.
    let foreign = SessionRecord {
        package_id: Uuid::from_u128(9),
        ..record
    };
    write_record(&f.root(), &foreign).unwrap();
    assert!(!f.sessions.reattach(PACKAGE, f.root()).await.unwrap());
    assert!(!record_path(&f.root()).exists());
    assert!(!f.sessions.is_running(PACKAGE));
    let (playtime, _) = db::installs::playtime(&f.db, PACKAGE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(playtime, 10);
}
