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
use vgames_desktop_lib::events::EventBus;
use vgames_desktop_lib::social::model::{
    Conversation, DeviceNotice, FriendList, Message, MessageBody, MessageStatus, Presence,
    SocialConnection, SocialConnectionState, UserSummary,
};
use vgames_desktop_lib::social::ports::{IdleSource, ServerSession, SessionSlot};
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
}

struct NoIdle;
impl IdleSource for NoIdle {
    fn idle_for(&self) -> Option<Duration> {
        None
    }
}

struct Launcher {
    name: &'static str,
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
    let service = SocialService::start(
        SessionSlot::new(),
        db,
        &EventBus::new(),
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
