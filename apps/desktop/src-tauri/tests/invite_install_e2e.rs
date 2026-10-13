//! GAME-12: a game invite on the real install path. The real API in process (PostgreSQL, fs
//! storage, realtime) publishes a real package; Bob's launcher is a full `AppState` with its own
//! download worker. Alice invites Bob to a game he lacks: Bob accepts, the install dialog is
//! requested, the real install runs (`installs::start`), Alice sees `installing` with progress,
//! `ready` only after `InstallFinished`, and Bob's launcher starts the installed game through
//! the real launcher before the invite reads `joined`. An invite to a game Bob has in an older
//! version asks for the update instead, and is ready only once the update finished.
//!
//! ```sh
//! VGAMES_TEST_DATABASE_URL=postgres://vgames:vgames-dev-only@localhost:5432/postgres \
//!   cargo test -p vgames-desktop --test invite_install_e2e -- --ignored --nocapture
//! ```
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

#[path = "support/install.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use support::{Api, Desktop, GAME_SCRIPT, PackageSpec};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_desktop_lib::db::Db;
use vgames_desktop_lib::db::download_jobs::JobKind;
use vgames_desktop_lib::events::{AppEvent, InstallOutcome};
use vgames_desktop_lib::events::{EventBus, PackageRef};
use vgames_desktop_lib::social::model::{
    Conversation, DeviceNotice, FriendList, Invite, InviteInstallReason, InviteState, Message,
    MessageStatus, Presence, SocialConnection, SocialConnectionState, UserSummary,
};
use vgames_desktop_lib::social::ports::{
    CatalogUpdates, IdleSource, LauncherGames, LocalLibrary, ServerSession, SessionSlot,
};
use vgames_desktop_lib::social::realtime::Timing;
use vgames_desktop_lib::social::secrets::MemoryStore;
use vgames_desktop_lib::social::service::{SocialEvents, SocialService};
use vgames_desktop_lib::social::store::Keys;
use zeroize::Zeroizing;

#[derive(Debug)]
enum Seen {
    Connected,
    InviteReceived(Invite),
    InviteChanged(Invite),
    InstallRequested(Uuid, PackageRef, InviteInstallReason),
    Other,
}

struct Recorder(mpsc::UnboundedSender<Seen>);

impl SocialEvents for Recorder {
    fn connection_changed(&self, c: &SocialConnection) {
        let _ = self.0.send(if c.state == SocialConnectionState::Connected {
            Seen::Connected
        } else {
            Seen::Other
        });
    }
    fn friends_changed(&self, _: &FriendList) {}
    fn presence_changed(&self, _: Uuid, _: &Presence) {}
    fn friend_request_received(&self, _: &UserSummary) {}
    fn conversations_changed(&self, _: &[Conversation]) {}
    fn message_received(&self, _: &Message) {}
    fn message_status_changed(&self, _: Uuid, _: Uuid, _: MessageStatus) {}
    fn typing(&self, _: Uuid, _: Uuid) {}
    fn device_notice(&self, _: Option<Uuid>, _: &DeviceNotice) {}
    fn invite_received(&self, i: &Invite) {
        let _ = self.0.send(Seen::InviteReceived(i.clone()));
    }
    fn invite_changed(&self, i: &Invite) {
        let _ = self.0.send(Seen::InviteChanged(i.clone()));
    }
    fn invite_install_requested(&self, id: Uuid, p: PackageRef, r: InviteInstallReason) {
        let _ = self.0.send(Seen::InstallRequested(id, p, r));
    }
}

struct NoIdle;
impl IdleSource for NoIdle {
    fn idle_for(&self) -> Option<Duration> {
        None
    }
}

struct Social {
    name: &'static str,
    service: SocialService,
    seen: mpsc::UnboundedReceiver<Seen>,
    shutdown: CancellationToken,
}

impl Social {
    /// A signed-in social service over `db` and `bus` (a whole launcher's, or a bare one).
    async fn start(
        name: &'static str,
        api: &Api,
        db: Db,
        bus: &EventBus,
        user: Uuid,
        token: String,
    ) -> Self {
        let (tx, seen) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();
        let service = SocialService::start(
            SessionSlot::new(),
            db,
            bus,
            Arc::new(Recorder(tx)),
            Arc::new(NoIdle),
            Arc::new(Keys::from_secret_store(&MemoryStore::default()).unwrap()),
            format!("{name}'s PC"),
            Timing {
                min_backoff: Duration::from_millis(50),
                max_backoff: Duration::from_millis(500),
                ..Timing::default()
            },
            shutdown.clone(),
        )
        .await
        .unwrap();
        service.sessions().set(Some(ServerSession {
            server_id: api.server_id,
            user_id: user,
            base_url: format!("{}/", api.url).parse().unwrap(),
            access_token: Arc::new(Zeroizing::new(token)),
        }));
        let mut s = Self {
            name,
            service,
            seen,
            shutdown,
        };
        s.wait(|e| matches!(e, Seen::Connected)).await;
        s.device().await;
        s
    }

    async fn wait(&mut self, pick: impl Fn(&Seen) -> bool) -> Seen {
        loop {
            let seen = tokio::time::timeout(Duration::from_secs(60), self.seen.recv())
                .await
                .unwrap_or_else(|_| panic!("{}: expected event did not arrive", self.name))
                .unwrap();
            if pick(&seen) {
                return seen;
            }
        }
    }

    async fn invite_state(&mut self, id: Uuid, state: InviteState) -> Invite {
        match self
            .wait(|s| matches!(s, Seen::InviteChanged(i) if i.id == id && i.state == state))
            .await
        {
            Seen::InviteChanged(i) => i,
            _ => unreachable!(),
        }
    }

    async fn device(&self) {
        for _ in 0..150 {
            if let Ok(list) = self.service.devices_list().await
                && list.iter().any(|d| d.current)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("{}: no registered device", self.name);
    }
}

/// Bob (a whole launcher with social) and Alice (social only), friends on one in-process API
/// that publishes `game` from `source`.
struct Scene {
    api: Api,
    desktop: Desktop,
    library_dir: std::path::PathBuf,
    library_id: Uuid,
    game: support::Published,
    source: std::path::PathBuf,
    alice: Social,
    bob: Social,
    bob_id: Uuid,
}

impl Scene {
    async fn start(work: &std::path::Path) -> Self {
        let api = Api::start().await;
        let source = work.join("source");
        PackageSpec::small().generate(&source);
        let game = api.publish_package("Arena", &source).await;
        let alice_id = api.owner_id;
        let (bob_id, bob_token) = api.player("bob", "100000000000000778").await;
        api.befriend(alice_id, bob_id).await;

        // Bob: a whole launcher (servers, catalog, downloads, installs, launcher) with social on
        // the same database and bus, and the real install state, update check and launcher
        // behind invites.
        let desktop = Desktop::open(&work.join("bob"), api.server_id, &bob_token).await;
        desktop.add_server(&api.url).await;
        let library_dir = work.join("Games");
        std::fs::create_dir_all(&library_dir).unwrap();
        let library_id =
            vgames_desktop_lib::db::libraries::add(&desktop.state.db, &library_dir, "Games")
                .await
                .unwrap()
                .root
                .id;
        desktop.state.downloads.start();
        let bob = Social::start(
            "bob",
            &api,
            desktop.state.db.clone(),
            &desktop.state.bus,
            bob_id,
            bob_token,
        )
        .await;
        bob.service.set_games(Arc::new(LauncherGames {
            library: LocalLibrary {
                db: desktop.state.db.clone(),
            },
            updates: Some(Arc::new(CatalogUpdates {
                catalog: Arc::clone(&desktop.state.catalog),
                db: desktop.state.db.clone(),
                installs: Arc::clone(&desktop.state.installs),
                bus: desktop.state.bus.clone(),
            })),
            launcher: Arc::clone(&desktop.state.launcher),
        }));

        // Alice: social only, on a bare database with the server known.
        let alice_db = Db::open_in_memory().unwrap();
        let (server_id, url) = (api.server_id, api.url.clone());
        alice_db
            .call(move |c| {
                c.execute(
                    "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at) VALUES (?1, ?2, 'Test', ?3, 'VG1-TEST', 0)",
                    vgames_desktop_lib::db::rusqlite::params![server_id.to_string(), url, vec![7u8; 32]],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let alice = Social::start(
            "alice",
            &api,
            alice_db,
            &EventBus::new(),
            alice_id,
            api.token.clone(),
        )
        .await;
        Self {
            api,
            desktop,
            library_dir,
            library_id,
            game,
            source,
            alice,
            bob,
            bob_id,
        }
    }

    fn package(&self) -> PackageRef {
        PackageRef {
            server_id: self.api.server_id,
            package_id: self.game.package_id,
        }
    }

    /// The install dialog's "Install": the real pipeline (signed manifest, download worker,
    /// verify).
    async fn install(&self) {
        vgames_desktop_lib::installs::start(
            &self.desktop.state.catalog,
            &self.desktop.state.downloads,
            self.package(),
            self.library_id,
        )
        .await
        .expect("install_start");
    }

    /// Alice invites Bob; Bob accepts and is asked to install for `reason`.
    async fn invite_needing(&mut self, reason: InviteInstallReason) -> Uuid {
        let sent = self
            .alice
            .service
            .invite_send(self.bob_id, self.game.package_id, Some("gg?".into()), None)
            .await
            .unwrap();
        self.bob
            .wait(|s| matches!(s, Seen::InviteReceived(i) if i.id == sent.id))
            .await;
        self.bob.service.invite_accept(sent.id).await.unwrap();
        match self
            .bob
            .wait(|s| matches!(s, Seen::InstallRequested(id, _, _) if *id == sent.id))
            .await
        {
            Seen::InstallRequested(_, p, got) => {
                assert_eq!(got, reason);
                assert_eq!(p, self.package());
            }
            _ => unreachable!(),
        }
        self.alice
            .invite_state(sent.id, InviteState::Accepted)
            .await;
        sent.id
    }

    async fn stop(self) {
        for s in [&self.alice, &self.bob] {
            s.shutdown.cancel();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.api.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn invite_install_ready_join_on_the_real_install_path() {
    let work = tempfile::tempdir().unwrap();
    let mut scene = Scene::start(work.path()).await;

    // The invite: Bob lacks the game, so accepting asks for the install dialog.
    let invite = scene.invite_needing(InviteInstallReason::Missing).await;
    scene.install().await;
    let installing = scene
        .alice
        .invite_state(invite, InviteState::Installing)
        .await;
    assert!(
        installing
            .progress
            .is_some_and(|p| (0.0..=1.0).contains(&p))
    );

    // Ready only after InstallFinished, then Bob's launcher starts the installed game.
    scene.alice.invite_state(invite, InviteState::Ready).await;
    scene.alice.invite_state(invite, InviteState::Joined).await;
    // `bin/game.sh` writes `launched.txt` next to itself.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(found) = walk_for(&scene.library_dir, "launched.txt") {
            eprintln!("the installed game ran: {}", found.display());
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the installed game never ran ({GAME_SCRIPT})"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    scene.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn an_invite_to_an_outdated_game_updates_it_first() {
    let work = tempfile::tempdir().unwrap();
    let mut scene = Scene::start(work.path()).await;
    let package = scene.package();

    // Bob has 1.0 installed; then 2.0 is released.
    let mut bus = scene.desktop.state.bus.subscribe();
    scene.install().await;
    loop {
        match tokio::time::timeout(Duration::from_secs(60), bus.recv()).await {
            Ok(Ok(AppEvent::InstallFinished(f))) if f.package == package => {
                assert!(matches!(f.outcome, InstallOutcome::Installed), "{f:?}");
                break;
            }
            Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            other => panic!("1.0 never finished installing: {other:?}"),
        }
    }
    std::fs::write(scene.source.join("bin/v2.txt"), b"2.0").unwrap();
    let v2 = scene
        .api
        .publish_version(scene.game.package_id, &scene.source, "2.0")
        .await;

    // Accepting asks for the update, not the install dialog, and the invite is not ready yet.
    let invite = scene.invite_needing(InviteInstallReason::Outdated).await;

    // The update dialog's "Update" (`install_update`): the real update job.
    vgames_desktop_lib::installs::actions::queue(
        &scene.desktop.state.db,
        package,
        JobKind::Update,
        false,
    )
    .await
    .expect("install_update");
    scene.desktop.state.downloads.queued();
    scene
        .alice
        .invite_state(invite, InviteState::Installing)
        .await;
    scene.alice.invite_state(invite, InviteState::Ready).await;
    let row = vgames_desktop_lib::db::installs::row(&scene.desktop.state.db, package)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.version_id, v2.version_id.to_string(), "joins on 2.0");
    scene.alice.invite_state(invite, InviteState::Joined).await;
    scene.stop().await;
}

/// The first file named `name` under `root`.
fn walk_for(root: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == name) {
                return Some(path);
            }
        }
    }
    None
}
