//! GAME-12: a game invite on the real install path. The real API in process (PostgreSQL, fs
//! storage, realtime) publishes a real package; Bob's launcher is a full `AppState` with its own
//! download worker. Alice invites Bob to a game he lacks: Bob accepts, the install dialog is
//! requested, the real install runs (`installs::start`), Alice sees `installing` with progress,
//! `ready` only after `InstallFinished`, and Bob's launcher starts the installed game through
//! the real launcher before the invite reads `joined`.
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
use vgames_desktop_lib::events::{EventBus, PackageRef};
use vgames_desktop_lib::social::model::{
    Conversation, DeviceNotice, FriendList, Invite, InviteInstallReason, InviteState, Message,
    MessageStatus, Presence, SocialConnection, SocialConnectionState, UserSummary,
};
use vgames_desktop_lib::social::ports::{
    IdleSource, LauncherGames, LocalLibrary, ServerSession, SessionSlot,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn invite_install_ready_join_on_the_real_install_path() {
    let work = tempfile::tempdir().unwrap();
    let api = Api::start().await;
    let source = work.path().join("source");
    PackageSpec::small().generate(&source);
    let game = api.publish_package("Arena", &source).await;
    let alice_id = api.owner_id;
    let (bob_id, bob_token) = api.player("bob", "100000000000000778").await;
    api.befriend(alice_id, bob_id).await;

    // Bob: a whole launcher (servers, catalog, downloads, installs, launcher) with social on
    // the same database and bus, and the real install state and launcher behind invites.
    let desktop = Desktop::open(&work.path().join("bob"), api.server_id, &bob_token).await;
    desktop.add_server(&api.url).await;
    let library_dir = work.path().join("Games");
    std::fs::create_dir_all(&library_dir).unwrap();
    let library_id =
        vgames_desktop_lib::db::libraries::add(&desktop.state.db, &library_dir, "Games")
            .await
            .unwrap()
            .root
            .id;
    desktop.state.downloads.start();
    let mut bob = Social::start(
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
    let mut alice = Social::start(
        "alice",
        &api,
        alice_db,
        &EventBus::new(),
        alice_id,
        api.token.clone(),
    )
    .await;

    // The invite: Bob lacks the game, so accepting asks for the install dialog.
    let sent = alice
        .service
        .invite_send(bob_id, game.package_id, Some("gg?".into()), None)
        .await
        .unwrap();
    bob.wait(|s| matches!(s, Seen::InviteReceived(i) if i.id == sent.id))
        .await;
    bob.service.invite_accept(sent.id).await.unwrap();
    let package = match bob
        .wait(|s| matches!(s, Seen::InstallRequested(id, _, _) if *id == sent.id))
        .await
    {
        Seen::InstallRequested(_, p, reason) => {
            assert_eq!(reason, InviteInstallReason::Missing);
            p
        }
        _ => unreachable!(),
    };
    assert_eq!(package.package_id, game.package_id);
    alice.invite_state(sent.id, InviteState::Accepted).await;

    // The dialog's "Install": the real pipeline (signed manifest, download worker, verify).
    vgames_desktop_lib::installs::start(
        &desktop.state.catalog,
        &desktop.state.downloads,
        package,
        library_id,
    )
    .await
    .expect("install_start");
    let installing = alice.invite_state(sent.id, InviteState::Installing).await;
    assert!(
        installing
            .progress
            .is_some_and(|p| (0.0..=1.0).contains(&p))
    );

    // Ready only after InstallFinished, then Bob's launcher starts the installed game.
    alice.invite_state(sent.id, InviteState::Ready).await;
    alice.invite_state(sent.id, InviteState::Joined).await;
    // `bin/game.sh` writes `launched.txt` next to itself.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(found) = walk_for(&library_dir, "launched.txt") {
            eprintln!("the installed game ran: {}", found.display());
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the installed game never ran ({GAME_SCRIPT})"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    for s in [&alice, &bob] {
        s.shutdown.cancel();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    api.stop().await;
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
