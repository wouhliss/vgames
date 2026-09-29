//! A4-T08 acceptance: launchers chat end to end through the **real** API (in process, on a
//! fresh PostgreSQL database): Alice and Bob exchange messages (pre-key, then normal Olm
//! messages); a second device of Bob joins and receives new messages but not old ones;
//! revoking it stops delivery to it; the server never holds a plaintext.
//!
//! Needs PostgreSQL 18 (or any version with `uuidv7()`):
//! `VGAMES_TEST_DATABASE_URL=postgres://vgames:vgames-dev-only@127.0.0.1:5432/postgres \
//!  cargo test -p vgames-desktop --test social_chat -- --ignored`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_desktop_lib::db::Db;
use vgames_desktop_lib::events::{
    AppEvent, EventBus, InstallFinished, InstallOutcome, InstallPhase, InstallProgress, PackageRef,
};
use vgames_desktop_lib::social::model::{
    Conversation, DeviceNotice, FriendList, Invite, InviteFailure, InviteInstallReason,
    InviteState, Message, MessageBody, MessageStatus, Presence, SocialConnection,
    SocialConnectionState, UserSummary,
};
use vgames_desktop_lib::social::ports::{
    BoxFuture, GameCheck, Games, IdleSource, LaunchError, ServerSession, SessionSlot,
};
use vgames_desktop_lib::social::realtime::Timing;
use vgames_desktop_lib::social::secrets::MemoryStore;
use vgames_desktop_lib::social::service::{SocialEvents, SocialService};
use vgames_desktop_lib::social::store::Keys;
use zeroize::Zeroizing;

const SERVER_ID: &str = "01920000-0000-7000-8000-0000000c4a70";

// ---- the API ----------------------------------------------------------------------------------

struct Api {
    pool: PgPool,
    base: String,
    admin: PgPool,
    db_name: String,
}

async fn start_api() -> Api {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let admin_url = std::env::var("VGAMES_TEST_DATABASE_URL")
        .expect("set VGAMES_TEST_DATABASE_URL to a PostgreSQL maintenance database URL");
    let admin = PgPool::connect(&admin_url).await.unwrap();
    let db_name = format!("vgames_chat_{}", Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db_name}")))
        .execute(&admin)
        .await
        .unwrap();
    let mut url = url::Url::parse(&admin_url).unwrap();
    url.set_path(&db_name);
    let pool = PgPool::connect(url.as_str()).await.unwrap();
    vgames_api::db::MIGRATOR.run(&pool).await.unwrap();

    let data = std::env::temp_dir().join(format!("vgames-chat-{}", Uuid::now_v7()));
    let env: HashMap<&str, String> = HashMap::from([
        ("VGAMES_PUBLIC_URL", "http://localhost:8080".to_owned()),
        ("VGAMES_SERVER_ID", SERVER_ID.to_owned()),
        ("DATABASE_URL", url.to_string()),
        (
            "VGAMES_ROOT_PUBLIC_KEY",
            "sn/b0D2mU+3RfBglb8/Jy/2BfTWHvE2SyIgXkx3m8U8=".to_owned(),
        ),
        (
            "VGAMES_SERVER_SECRET",
            "dGVzdC1zZXJ2ZXItc2VjcmV0LXRlc3Qtc2VydmVyLXNlY3JldA==".to_owned(),
        ),
        ("VGAMES_DEV_FAKE_DISCORD", "true".to_owned()),
        ("VGAMES_STORAGE_BACKEND", "fs".to_owned()),
        ("VGAMES_FS_STORAGE_ROOT", data.display().to_string()),
        (
            "VGAMES_FS_URL_SIGNING_KEY",
            "dGVzdC1mcy1zaWduaW5nLWtleS10ZXN0LWZzLXNpZ25pbmc=".to_owned(),
        ),
    ]);
    let config = vgames_api::Config::from_lookup(|k| env.get(k).cloned()).unwrap();
    let state = vgames_api::AppState::new(config, pool.clone()).unwrap();
    vgames_api::realtime::start(&state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(vgames_api::server::serve_on(listener, state.clone()));
    let mut ready = state.realtime_ready.subscribe();
    tokio::time::timeout(Duration::from_secs(10), ready.wait_for(|r| *r))
        .await
        .unwrap()
        .unwrap();
    Api {
        pool,
        base: format!("http://{addr}/"),
        admin,
        db_name,
    }
}

impl Api {
    async fn user(&self, name: &str) -> Uuid {
        let discord = format!(
            "{}",
            700_000_000_000_000_000u64 + u64::from(name.as_bytes()[0])
        );
        sqlx::query_scalar("INSERT INTO users (discord_id, username) VALUES ($1, $2) RETURNING id")
            .bind(discord)
            .bind(name)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// A desktop session (what signing in on a launcher creates); returns its access token.
    async fn session(&self, user: Uuid) -> String {
        use sha2::{Digest, Sha256};
        let mut raw = [0u8; 32];
        getrandom::fill(&mut raw).unwrap();
        let token = format!(
            "vga_{}",
            base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, raw)
        );
        let mut refresh = [0u8; 32];
        getrandom::fill(&mut refresh).unwrap();
        sqlx::query(
            "INSERT INTO sessions (user_id, kind, access_token_hash, access_expires_at, refresh_token_hash, refresh_expires_at)
             VALUES ($1, 'desktop', $2, now() + interval '1 hour', $3, now() + interval '30 days')",
        )
        .bind(user)
        .bind(Sha256::digest(token.as_bytes()).to_vec())
        .bind(Sha256::digest(refresh).to_vec())
        .execute(&self.pool)
        .await
        .unwrap();
        token
    }

    async fn befriend(&self, a: Uuid, b: Uuid) {
        let (low, high) = if a < b { (a, b) } else { (b, a) };
        sqlx::query(
            "INSERT INTO friendships (user_low, user_high, state, requested_by, accepted_at)
             VALUES ($1, $2, 'accepted', $1, now())",
        )
        .bind(low)
        .bind(high)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn now(&self) -> time::OffsetDateTime {
        sqlx::query_scalar("SELECT now()")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// Envelopes addressed to `device` since `since` (acknowledged ones are deleted, so
    /// this counts what the relay accepted for it and has not delivered).
    async fn envelopes_to_since(&self, device: Uuid, since: time::OffsetDateTime) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM message_envelopes WHERE recipient_device_id = $1 AND created_at >= $2",
        )
        .bind(device)
        .bind(since)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn drop_database(self) {
        self.pool.close().await;
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            self.db_name
        )))
        .execute(&self.admin)
        .await;
    }
}

// ---- launchers --------------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Seen {
    Connection(SocialConnection),
    Received(Message),
    Status(Uuid, MessageStatus),
    Notice(Option<Uuid>, DeviceNotice),
    Typing(Uuid, Uuid),
    InviteReceived(Invite),
    InviteChanged(Invite),
    InstallRequested(Uuid, PackageRef, InviteInstallReason),
    Other,
}

struct Recorder(mpsc::UnboundedSender<Seen>);

impl SocialEvents for Recorder {
    fn connection_changed(&self, c: &SocialConnection) {
        let _ = self.0.send(Seen::Connection(c.clone()));
    }
    fn friends_changed(&self, _: &FriendList) {
        let _ = self.0.send(Seen::Other);
    }
    fn presence_changed(&self, _: Uuid, _: &Presence) {}
    fn friend_request_received(&self, _: &UserSummary) {}
    fn conversations_changed(&self, _: &[Conversation]) {}
    fn message_received(&self, m: &Message) {
        let _ = self.0.send(Seen::Received(m.clone()));
    }
    fn message_status_changed(&self, id: Uuid, _: Uuid, status: MessageStatus) {
        let _ = self.0.send(Seen::Status(id, status));
    }
    fn typing(&self, conversation: Uuid, user: Uuid) {
        let _ = self.0.send(Seen::Typing(conversation, user));
    }
    fn device_notice(&self, c: Option<Uuid>, n: &DeviceNotice) {
        let _ = self.0.send(Seen::Notice(c, n.clone()));
    }
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

struct Launcher {
    name: &'static str,
    bus: EventBus,
    service: SocialService,
    seen: mpsc::UnboundedReceiver<Seen>,
    shutdown: CancellationToken,
}

async fn launcher(api: &Api, name: &'static str, user: Uuid) -> Launcher {
    let token = api.session(user).await;
    let db = Db::open_in_memory().unwrap();
    let server_id: Uuid = SERVER_ID.parse().unwrap();
    let base = api.base.clone();
    db.call(move |c| {
        c.execute(
            "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at) VALUES (?1, ?2, 'Test', ?3, 'VG1-TEST', 0)",
            rusqlite::params![server_id.to_string(), base, vec![7u8; 32]],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let keys = Keys::from_secret_store(&MemoryStore::default()).unwrap();
    let (tx, seen) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let bus = EventBus::new();
    let service = SocialService::start(
        SessionSlot::new(),
        db,
        &bus,
        Arc::new(Recorder(tx)),
        Arc::new(NoIdle),
        Arc::new(keys),
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
        server_id,
        user_id: user,
        base_url: api.base.parse().unwrap(),
        access_token: Arc::new(Zeroizing::new(token)),
    }));
    let mut l = Launcher {
        name,
        bus,
        service,
        seen,
        shutdown,
    };
    l.wait(|s| matches!(s, Seen::Connection(c) if c.state == SocialConnectionState::Connected))
        .await;
    l
}

impl Launcher {
    async fn wait(&mut self, pick: impl Fn(&Seen) -> bool) -> Seen {
        loop {
            let seen = tokio::time::timeout(Duration::from_secs(15), self.seen.recv())
                .await
                .unwrap_or_else(|_| panic!("{}: expected event did not arrive", self.name))
                .unwrap();
            if pick(&seen) {
                return seen;
            }
        }
    }

    /// Waits until this launcher's device is registered with its one-time keys.
    async fn device(&self) -> Uuid {
        for _ in 0..150 {
            if let Ok(list) = self.service.devices_list().await
                && let Some(d) = list.into_iter().find(|d| d.current)
            {
                return d.id;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("{}: no registered device", self.name);
    }

    async fn received_text(&mut self, text: &str) -> Message {
        let want = text.to_owned();
        match self
            .wait(|s| matches!(s, Seen::Received(m) if m.body == MessageBody::Text { text: want.clone() }))
            .await
        {
            Seen::Received(m) => m,
            _ => unreachable!(),
        }
    }

    async fn send(&mut self, conversation: Uuid, text: &str) -> Message {
        let m = self
            .service
            .message_send(conversation, text.to_owned())
            .await
            .unwrap();
        assert_eq!(m.status, MessageStatus::Pending);
        let id = m.id;
        self.wait(|s| matches!(s, Seen::Status(i, MessageStatus::Sent) if *i == id))
            .await;
        m
    }

    async fn texts(&self, conversation: Uuid) -> Vec<(bool, String)> {
        self.service
            .messages_list(conversation, None, 200)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|m| match m.body {
                MessageBody::Text { text } => Some((m.mine, text)),
                _ => None,
            })
            .collect()
    }
}

// ---- the scenario -------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn two_launchers_chat_and_a_new_device_gets_only_new_messages() {
    let api = start_api().await;
    let alice_id = api.user("alice").await;
    let bob_id = api.user("bob").await;
    api.befriend(alice_id, bob_id).await;

    let mut alice = launcher(&api, "alice", alice_id).await;
    let mut bob = launcher(&api, "bob", bob_id).await;
    let _alice_device = alice.device().await;
    let _bob_device = bob.device().await;

    // Alice opens the direct conversation and writes first (pre-key message).
    let conv = alice
        .service
        .conversation_open_direct(bob_id)
        .await
        .unwrap();
    alice.send(conv.id, "hello bob").await;
    let got = bob.received_text("hello bob").await;
    assert!(!got.mine);
    assert_eq!(got.sender_user_id, alice_id);
    assert_eq!(got.conversation_id, conv.id);

    // Bob answers (normal messages from here on), and both keep talking.
    let bob_convs = bob.service.conversations_list().await.unwrap();
    assert_eq!(bob_convs.len(), 1);
    assert_eq!(bob_convs[0].unread, 1);
    bob.service.conversation_mark_read(conv.id).await.unwrap();
    assert_eq!(bob.service.conversations_list().await.unwrap()[0].unread, 0);
    bob.send(conv.id, "hi alice").await;
    alice.received_text("hi alice").await;
    alice.send(conv.id, "old news").await;
    bob.received_text("old news").await;

    // Typing reaches the other side (and is throttled on the sender).
    alice.service.typing_start(conv.id).unwrap();
    alice.service.typing_start(conv.id).unwrap();
    bob.wait(|s| matches!(s, Seen::Typing(c, u) if *c == conv.id && *u == alice_id))
        .await;

    // Safety numbers agree on both sides.
    let a_sn = alice.service.contact_security(bob_id).await.unwrap();
    let b_sn = bob.service.contact_security(alice_id).await.unwrap();
    assert_eq!(a_sn.safety_number, b_sn.safety_number);
    assert_eq!(a_sn.safety_number.len(), 60);

    // Bob signs in on a second computer.
    let mut bob2 = launcher(&api, "bob2", bob_id).await;
    let bob2_device = bob2.device().await;
    // Alice is told about Bob's new device in the conversation.
    let notice = alice
        .wait(|s| matches!(s, Seen::Notice(_, DeviceNotice::NewDevice { device_id, .. }) if *device_id == bob2_device))
        .await;
    assert!(matches!(notice, Seen::Notice(Some(c), _) if c == conv.id));
    // The safety number changed with Bob's device list.
    let a_sn2 = alice.service.contact_security(bob_id).await.unwrap();
    assert_ne!(a_sn2.safety_number, a_sn.safety_number);

    // New messages reach both of Bob's devices; Bob's own messages reach his other device.
    alice.send(conv.id, "new for both").await;
    bob.received_text("new for both").await;
    bob2.received_text("new for both").await;
    bob.send(conv.id, "from bob's first pc").await;
    alice.received_text("from bob's first pc").await;
    let copy = bob2.received_text("from bob's first pc").await;
    assert!(copy.mine, "a copy of Bob's own message is his");

    // The new device has only what was sent after it joined.
    let on_bob2 = bob2.texts(conv.id).await;
    assert_eq!(
        on_bob2,
        vec![
            (false, "new for both".to_owned()),
            (true, "from bob's first pc".to_owned())
        ]
    );
    let on_bob = bob.texts(conv.id).await;
    assert_eq!(on_bob.len(), 5, "{on_bob:?}");

    // Bob revokes the second device from the first one: nothing reaches it any more.
    bob.service.device_revoke(bob2_device).await.unwrap();
    let revoked_at = api.now().await;
    alice
        .wait(|s| matches!(s, Seen::Notice(_, DeviceNotice::DeviceRevoked { device_id, .. }) if *device_id == bob2_device))
        .await;
    alice.send(conv.id, "after the revocation").await;
    bob.received_text("after the revocation").await;
    bob.send(conv.id, "bob again").await;
    alice.received_text("bob again").await;
    assert_eq!(
        api.envelopes_to_since(bob2_device, revoked_at).await,
        0,
        "no envelope was addressed to the revoked device"
    );

    // The server only ever held ciphertext.
    for text in [
        "hello bob",
        "hi alice",
        "new for both",
        "after the revocation",
    ] {
        let hits: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM message_envelopes WHERE position(convert_to($1, 'UTF8') in ciphertext) > 0",
        )
        .bind(text)
        .fetch_one(&api.pool)
        .await
        .unwrap();
        assert_eq!(hits, 0, "plaintext {text:?} found on the server");
    }

    for l in [&alice, &bob, &bob2] {
        l.shutdown.cancel();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    api.drop_database().await;
}

// ---- A4-T09: the invite handshake (M3 demo) ---------------------------------------------------

impl Api {
    /// A published package with one release, as the catalog would have it.
    async fn package(&self, slug: &str, by: Uuid) -> Uuid {
        let package: Uuid = sqlx::query_scalar(
            "INSERT INTO packages (slug, title, status, created_by) VALUES ($1, $1, 'published', $2) RETURNING id",
        )
        .bind(slug)
        .bind(by)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO trust_bundles (version, bundle, signature) VALUES (1, '\\x00', $1) ON CONFLICT DO NOTHING")
            .bind(vec![0u8; 64])
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO publisher_keys (key_id, public_key, holder_user_id, label, not_before, not_after, trust_version)
             VALUES ('0123456789abcdef0123456789abcdef', $1, $2, 'test', now() - interval '1 day', now() + interval '1 year', 1)
             ON CONFLICT DO NOTHING",
        )
        .bind(vec![7u8; 32])
        .bind(by)
        .execute(&self.pool)
        .await
        .unwrap();
        let version: Uuid = sqlx::query_scalar(
            "INSERT INTO package_versions (package_id, platform, sequence, version_label, state, pack_count, total_size,
                file_count, chunk_count, manifest_object, manifest_size, manifest_blake3, signature, publisher_key_id,
                created_by, published_at)
             VALUES ($1, 'linux-x86_64', 1, '1.0', 'published', 1, 1234, 1, 1, 'm', 10, $2, $3,
                '0123456789abcdef0123456789abcdef', $4, now())
             RETURNING id",
        )
        .bind(package)
        .bind(vec![1u8; 32])
        .bind(vec![2u8; 64])
        .bind(by)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO package_releases (package_id, platform, version_id, updated_by) VALUES ($1, 'linux-x86_64', $2, $3)")
            .bind(package)
            .bind(version)
            .bind(by)
            .execute(&self.pool)
            .await
            .unwrap();
        package
    }
}

/// Agent 2's library and launcher, faked: a fixed install state and a record of launches.
struct FakeGames {
    check: std::sync::Mutex<GameCheck>,
    launches: mpsc::UnboundedSender<(Uuid, Option<String>)>,
}

impl Games for FakeGames {
    fn check(&self, _: Uuid, _: Uuid) -> BoxFuture<'_, GameCheck> {
        let c = *self.check.lock().unwrap();
        Box::pin(async move { c })
    }
    fn launch_join(
        &self,
        _: Uuid,
        package: Uuid,
        join_secret: Option<String>,
    ) -> BoxFuture<'_, Result<(), LaunchError>> {
        let _ = self.launches.send((package, join_secret));
        Box::pin(async { Ok(()) })
    }
}

impl Launcher {
    async fn invite_state(&mut self, id: Uuid, want: InviteState) -> Invite {
        match self
            .wait(|s| matches!(s, Seen::InviteChanged(i) if i.id == id && i.state == want))
            .await
        {
            Seen::InviteChanged(i) => i,
            _ => unreachable!(),
        }
    }

    fn install_progress(&self, package: PackageRef, done: u64, phase: InstallPhase) {
        self.bus.publish(AppEvent::InstallProgress(InstallProgress {
            package,
            phase,
            bytes_done: done,
            bytes_total: 100,
            bytes_per_second: 10,
            eta_seconds: None,
            connections: 1,
        }));
    }

    fn install_finished(&self, package: PackageRef, outcome: InstallOutcome) {
        self.bus.publish(AppEvent::InstallFinished(InstallFinished {
            package,
            outcome,
        }));
    }
}

async fn next_launch(
    rx: &mut mpsc::UnboundedReceiver<(Uuid, Option<String>)>,
) -> (Uuid, Option<String>) {
    tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .expect("a launch in time")
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: set VGAMES_TEST_DATABASE_URL"]
async fn invite_install_ready_join_handshake() {
    let api = start_api().await;
    let alice_id = api.user("alice").await;
    let bob_id = api.user("bob").await;
    api.befriend(alice_id, bob_id).await;
    let game = api.package("arena", alice_id).await;
    let other = api.package("racer", alice_id).await;

    let mut alice = launcher(&api, "alice", alice_id).await;
    let mut bob = launcher(&api, "bob", bob_id).await;
    alice.device().await;
    bob.device().await;
    let (launch_tx, mut launches) = mpsc::unbounded_channel();
    let games = Arc::new(FakeGames {
        check: std::sync::Mutex::new(GameCheck::Missing),
        launches: launch_tx,
    });
    bob.service.set_games(games.clone());

    // 1. Missing game: invite with a join secret → accept → install dialog at once →
    //    progress → ready → invite.join over Olm → launch with the secret → joined.
    let sent = alice
        .service
        .invite_send(
            bob_id,
            game,
            Some("gg?".into()),
            Some(" 10.0.0.2:27015 ".into()),
        )
        .await
        .unwrap();
    assert!(sent.has_join_secret);
    assert_eq!(sent.state, InviteState::Pending);
    let got = match bob
        .wait(|s| matches!(s, Seen::InviteReceived(i) if i.id == sent.id))
        .await
    {
        Seen::InviteReceived(i) => i,
        _ => unreachable!(),
    };
    assert!(
        !got.has_join_secret,
        "the secret never reaches the invitee's UI model"
    );
    assert_eq!(got.package.id, game);
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
    assert_eq!(package.package_id, game);
    alice.invite_state(sent.id, InviteState::Accepted).await;
    // The normal install path runs (signature check first); progress is reported.
    bob.install_progress(package, 0, InstallPhase::VerifyingManifest);
    bob.install_progress(package, 30, InstallPhase::Downloading);
    let installing = alice.invite_state(sent.id, InviteState::Installing).await;
    assert!(
        installing
            .progress
            .is_some_and(|p| (0.0..=1.0).contains(&p))
    );
    // Not ready before the install finished.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let now = alice.service.invites_list().await.unwrap();
    assert_eq!(
        now.iter().find(|i| i.id == sent.id).unwrap().state,
        InviteState::Installing
    );
    assert!(launches.try_recv().is_err());

    *games.check.lock().unwrap() = GameCheck::Current;
    bob.install_finished(package, InstallOutcome::Installed);
    alice.invite_state(sent.id, InviteState::Ready).await;
    let (launched, secret) = next_launch(&mut launches).await;
    assert_eq!(launched, game);
    assert_eq!(secret.as_deref(), Some("10.0.0.2:27015"));
    alice.invite_state(sent.id, InviteState::Joined).await;
    // The server never held the secret.
    let hits: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM game_invites WHERE message LIKE '%10.0.0.2%')
              + (SELECT count(*) FROM message_envelopes WHERE position(convert_to('10.0.0.2', 'UTF8') in ciphertext) > 0)",
    )
    .fetch_one(&api.pool)
    .await
    .unwrap();
    assert_eq!(hits, 0);

    // 2. Installed and current, no secret: ready at once, normal launch without arguments.
    let sent = alice
        .service
        .invite_send(bob_id, other, None, None)
        .await
        .unwrap();
    assert!(!sent.has_join_secret);
    bob.wait(|s| matches!(s, Seen::InviteReceived(i) if i.id == sent.id))
        .await;
    bob.service.invite_accept(sent.id).await.unwrap();
    alice.invite_state(sent.id, InviteState::Ready).await;
    let (launched, secret) = next_launch(&mut launches).await;
    assert_eq!((launched, secret), (other, None));
    alice.invite_state(sent.id, InviteState::Joined).await;

    // 3. The install fails its signature check: failed, never ready, nothing launched.
    *games.check.lock().unwrap() = GameCheck::Missing;
    let sent = alice
        .service
        .invite_send(bob_id, game, None, Some("lobby-7".into()))
        .await
        .unwrap();
    bob.wait(|s| matches!(s, Seen::InviteReceived(i) if i.id == sent.id))
        .await;
    bob.service.invite_accept(sent.id).await.unwrap();
    bob.wait(|s| matches!(s, Seen::InstallRequested(id, _, _) if *id == sent.id))
        .await;
    bob.install_finished(
        package,
        InstallOutcome::Failed {
            code: "signature_invalid".into(),
            message: "The package signature does not verify".into(),
        },
    );
    let failed = alice.invite_state(sent.id, InviteState::Failed).await;
    assert_eq!(failed.failure_reason, Some(InviteFailure::InstallFailed));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(launches.try_recv().is_err());

    // 4. A pending invite cancelled by the sender reaches the invitee as cancelled.
    let sent = alice
        .service
        .invite_send(bob_id, other, None, None)
        .await
        .unwrap();
    bob.wait(|s| matches!(s, Seen::InviteReceived(i) if i.id == sent.id))
        .await;
    alice.service.invite_cancel(sent.id).await.unwrap();
    bob.invite_state(sent.id, InviteState::Cancelled).await;
    assert!(matches!(
        bob.service.invite_accept(sent.id).await,
        Err(vgames_desktop_lib::social::model::SocialError::Conflict { .. })
    ));
    // An invalid secret is refused before anything is sent.
    assert!(matches!(
        alice
            .service
            .invite_send(bob_id, other, None, Some("a b".into()))
            .await,
        Err(vgames_desktop_lib::social::model::SocialError::InvalidInput { .. })
    ));

    for l in [&alice, &bob] {
        l.shutdown.cancel();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    api.drop_database().await;
}
