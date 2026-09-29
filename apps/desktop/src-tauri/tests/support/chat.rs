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
    /// The test database and file storage every instance of this API shares.
    url: String,
    data: std::path::PathBuf,
    /// The first instance (see [`Api::stop`] / [`Api::restart`]).
    state: vgames_api::AppState,
    addr: std::net::SocketAddr,
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
    let (state, addr) = serve(url.as_str(), &data, None).await;
    Api {
        pool,
        base: format!("http://{addr}/"),
        admin,
        db_name,
        url: url.to_string(),
        data,
        state,
        addr,
    }
}

/// One API instance on the test database, on `addr` (retried while the old one closes) or a
/// fresh port. Returns once it `LISTEN`s for realtime events.
async fn serve(
    url: &str,
    data: &std::path::Path,
    addr: Option<std::net::SocketAddr>,
) -> (vgames_api::AppState, std::net::SocketAddr) {
    let env: HashMap<&str, String> = HashMap::from([
        ("VGAMES_PUBLIC_URL", "http://localhost:8080".to_owned()),
        ("VGAMES_SERVER_ID", SERVER_ID.to_owned()),
        ("DATABASE_URL", url.to_owned()),
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
    let pool = PgPool::connect(url).await.unwrap();
    let state = vgames_api::AppState::new(config, pool).unwrap();
    vgames_api::realtime::start(&state);
    let bind = addr.unwrap_or_else(|| "127.0.0.1:0".parse().unwrap());
    let mut tries = 0;
    let listener = loop {
        match tokio::net::TcpListener::bind(bind).await {
            Ok(l) => break l,
            Err(e) if tries < 50 => {
                let _ = e;
                tries += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => panic!("cannot bind {bind}: {e}"),
        }
    };
    let addr = listener.local_addr().unwrap();
    tokio::spawn(vgames_api::server::serve_on(listener, state.clone()));
    let mut ready = state.realtime_ready.subscribe();
    tokio::time::timeout(Duration::from_secs(10), ready.wait_for(|r| *r))
        .await
        .unwrap()
        .unwrap();
    (state, addr)
}

impl Api {
    /// Stops the first instance like a crash-free restart does: sockets closed with 1012,
    /// requests drained, the port released.
    fn stop(&self) {
        self.state.shutdown.cancel();
    }

    /// Starts the first instance again on the same port.
    async fn restart(&mut self) {
        let (state, _) = serve(&self.url, &self.data, Some(self.addr)).await;
        self.state = state;
    }

    /// Another instance on the same database (another port): its base URL and state.
    async fn another_instance(&self) -> (String, vgames_api::AppState) {
        let (state, addr) = serve(&self.url, &self.data, None).await;
        (format!("http://{addr}/"), state)
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
    launcher_at(api, &api.base.clone(), name, user).await
}

/// A launcher signed in to the API instance at `base`.
async fn launcher_at(api: &Api, base: &str, name: &'static str, user: Uuid) -> Launcher {
    launcher_with_db(api, base, name, user, Db::open_in_memory().unwrap()).await
}

/// Same, with the launcher's database at a given place (on disk for long runs).
async fn launcher_with_db(
    api: &Api,
    base: &str,
    name: &'static str,
    user: Uuid,
    db: Db,
) -> Launcher {
    let token = api.session(user).await;
    let server_id: Uuid = SERVER_ID.parse().unwrap();
    let row_url = base.to_owned();
    db.call(move |c| {
        c.execute(
            "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at) VALUES (?1, ?2, 'Test', ?3, 'VG1-TEST', 0)",
            rusqlite::params![server_id.to_string(), row_url, vec![7u8; 32]],
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
        base_url: base.parse().unwrap(),
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
