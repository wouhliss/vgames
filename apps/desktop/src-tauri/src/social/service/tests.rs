//! A4-T07: the social core against a fake gateway that behaves like the API's (tickets,
//! `hello` first, pushed events, close codes), over real sockets.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::*;
use crate::events::{GameExit, GameStarted, GameStopped, PackageRef};
use crate::social::model::{PresenceStatus, SocialConnectionState};
use crate::social::ports::{ServerSession, SessionSlot};

// ---- fake gateway ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Control {
    Event(String, Value),
    Close(u16),
    Drop,
    Silence,
}

struct Gateway {
    token: String,
    user_id: Uuid,
    control: broadcast::Sender<Control>,
    frames: mpsc::UnboundedSender<Value>,
    tickets: AtomicUsize,
    connections: AtomicUsize,
    friends: Mutex<Value>,
    requests: Mutex<Vec<Value>>,
    valid_tickets: Mutex<Vec<String>>,
}

type Gw = Arc<Gateway>;

fn authorized(gw: &Gateway, headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {}", gw.token))
}

async fn ticket(State(gw): State<Gw>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&gw, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"code": "unauthenticated", "title": "no"})),
        )
            .into_response();
    }
    let n = gw.tickets.fetch_add(1, Ordering::SeqCst);
    let t = format!("ticket-{n}");
    gw.valid_tickets.lock().unwrap().push(t.clone());
    (
        StatusCode::CREATED,
        axum::Json(json!({"ticket": t, "expires_at": "2099-01-01T00:00:00Z"})),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct TicketQuery {
    ticket: String,
}

async fn socket(
    State(gw): State<Gw>,
    Query(q): Query<TicketQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let ok = {
        let mut valid = gw.valid_tickets.lock().unwrap();
        let before = valid.len();
        valid.retain(|t| *t != q.ticket);
        valid.len() < before
    };
    if !ok {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| run_socket(gw, socket))
}

async fn run_socket(gw: Gw, mut socket: WebSocket) {
    gw.connections.fetch_add(1, Ordering::SeqCst);
    let mut control = gw.control.subscribe();
    let hello = json!({"v": 1, "type": "hello", "data": {"user_id": gw.user_id, "server_time": "2026-09-26T10:00:00Z"}});
    if socket
        .send(WsMessage::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    let mut silent = false;
    loop {
        tokio::select! {
            c = control.recv() => match c {
                Ok(Control::Event(kind, data)) => {
                    let frame = json!({"v": 1, "type": kind, "data": data});
                    if socket.send(WsMessage::Text(frame.to_string().into())).await.is_err() { return; }
                }
                Ok(Control::Close(code)) => {
                    let _ = socket.send(WsMessage::Close(Some(CloseFrame { code, reason: "bye".into() }))).await;
                    // Like the API: finish the close handshake instead of resetting the
                    // connection (a reset can discard the close frame before it is read).
                    let _ = tokio::time::timeout(Duration::from_secs(2), async {
                        while let Some(Ok(_)) = socket.recv().await {}
                    })
                    .await;
                    return;
                }
                Ok(Control::Drop) => return,
                Ok(Control::Silence) => silent = true,
                Err(_) => return,
            },
            m = socket.recv(), if !silent => match m {
                Some(Ok(WsMessage::Text(t))) => {
                    let _ = gw.frames.send(serde_json::from_str(t.as_str()).unwrap());
                }
                Some(Ok(_)) => {}
                _ => return,
            },
        }
    }
}

async fn friends(State(gw): State<Gw>, headers: HeaderMap) -> impl IntoResponse {
    if !authorized(&gw, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    axum::Json(gw.friends.lock().unwrap().clone()).into_response()
}

async fn friend_request(
    State(gw): State<Gw>,
    axum::Json(body): axum::Json<Value>,
) -> impl IntoResponse {
    gw.requests.lock().unwrap().push(body);
    let friend =
        json!({"user": {"id": Uuid::from_u128(77), "username": "sam"}, "state": "outgoing"});
    (StatusCode::CREATED, axum::Json(friend))
}

async fn block(State(gw): State<Gw>, Path(id): Path<Uuid>) -> StatusCode {
    gw.requests.lock().unwrap().push(json!({"block": id}));
    StatusCode::NO_CONTENT
}

struct Server {
    gw: Gw,
    addr: SocketAddr,
    frames: mpsc::UnboundedReceiver<Value>,
    stop: CancellationToken,
}

impl Server {
    async fn start(token: &str, port: u16) -> Self {
        let (frames_tx, frames) = mpsc::unbounded_channel();
        let gw = Arc::new(Gateway {
            token: token.to_owned(),
            user_id: Uuid::from_u128(1),
            control: broadcast::channel(64).0,
            frames: frames_tx,
            tickets: AtomicUsize::new(0),
            connections: AtomicUsize::new(0),
            friends: Mutex::new(json!({"friends": [], "incoming": [], "outgoing": []})),
            requests: Mutex::new(Vec::new()),
            valid_tickets: Mutex::new(Vec::new()),
        });
        let (addr, stop) = Self::serve(gw.clone(), port).await;
        Self {
            gw,
            addr,
            frames,
            stop,
        }
    }

    async fn serve(gw: Gw, port: u16) -> (SocketAddr, CancellationToken) {
        let app = Router::new()
            .route("/v1/realtime/ticket", post(ticket))
            .route("/v1/realtime", get(socket))
            .route("/v1/friends", get(friends))
            .route("/v1/friends/requests", post(friend_request))
            .route("/v1/blocks/{id}", post(block).merge(delete(block)))
            .with_state(gw);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = CancellationToken::new();
        let s = stop.clone();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { s.cancelled().await })
                .await
                .unwrap();
        });
        (addr, stop)
    }

    /// Stops accepting and drops every socket.
    async fn stop(&self) {
        self.stop.cancel();
        let _ = self.gw.control.send(Control::Drop);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    async fn restart(&mut self) {
        let (addr, stop) = Self::serve(self.gw.clone(), self.addr.port()).await;
        assert_eq!(addr, self.addr);
        self.stop = stop;
    }

    fn push(&self, kind: &str, data: Value) {
        let _ = self.gw.control.send(Control::Event(kind.to_owned(), data));
    }

    fn session(&self, server_id: Uuid) -> ServerSession {
        ServerSession {
            server_id,
            user_id: self.gw.user_id,
            base_url: format!("http://{}/", self.addr).parse().unwrap(),
            access_token: Arc::new(Zeroizing::new(self.gw.token.clone())),
        }
    }

    /// Next `presence.set` the launcher sent.
    async fn next_presence(&mut self) -> Value {
        loop {
            let f = tokio::time::timeout(Duration::from_secs(5), self.frames.recv())
                .await
                .expect("a frame in time")
                .unwrap();
            if f["type"] == "presence.set" {
                return f["data"].clone();
            }
        }
    }

    async fn no_presence_for(&mut self, d: Duration) {
        if let Ok(Some(f)) = tokio::time::timeout(d, self.frames.recv()).await {
            panic!("unexpected frame {f}");
        }
    }
}

// ---- recording events -----------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Seen {
    Connection(SocialConnection),
    Friends(FriendList),
    Presence(Uuid, Presence),
    Request(UserSummary),
}

struct Recorder(mpsc::UnboundedSender<Seen>);

impl SocialEvents for Recorder {
    fn connection_changed(&self, c: &SocialConnection) {
        let _ = self.0.send(Seen::Connection(c.clone()));
    }
    fn friends_changed(&self, f: &FriendList) {
        let _ = self.0.send(Seen::Friends(f.clone()));
    }
    fn presence_changed(&self, u: Uuid, p: &Presence) {
        let _ = self.0.send(Seen::Presence(u, p.clone()));
    }
    fn friend_request_received(&self, u: &UserSummary) {
        let _ = self.0.send(Seen::Request(u.clone()));
    }
    fn conversations_changed(&self, _: &[Conversation]) {}
    fn message_received(&self, _: &Message) {}
    fn message_status_changed(&self, _: Uuid, _: Uuid, _: MessageStatus) {}
    fn typing(&self, _: Uuid, _: Uuid) {}
    fn device_notice(&self, _: Option<Uuid>, _: &DeviceNotice) {}
}

fn test_keys() -> Keys {
    use crate::social::crypto::SecretKey32;
    Keys::new(
        SecretKey32::from_bytes([1; 32]),
        &SecretKey32::from_bytes([2; 32]),
    )
    .unwrap()
}

struct Idle(Mutex<Option<Duration>>);
impl IdleSource for Idle {
    fn idle_for(&self) -> Option<Duration> {
        *self.0.lock().unwrap()
    }
}

struct Harness {
    service: SocialService,
    seen: mpsc::UnboundedReceiver<Seen>,
    bus: EventBus,
    idle: Arc<Idle>,
    db: Db,
    shutdown: CancellationToken,
}

fn timing() -> Timing {
    Timing {
        min_backoff: Duration::from_millis(50),
        max_backoff: Duration::from_millis(200),
        dead_after: Duration::from_secs(60),
        connect_timeout: Duration::from_secs(5),
        idle_poll: Duration::from_millis(50),
        network_poll: Duration::from_secs(3600),
    }
}

async fn harness_with(timing: Timing) -> Harness {
    let (tx, seen) = mpsc::unbounded_channel();
    let db = Db::open_in_memory().unwrap();
    let bus = EventBus::new();
    let idle = Arc::new(Idle(Mutex::new(Some(Duration::ZERO))));
    let shutdown = CancellationToken::new();
    let service = SocialService::start(
        SessionSlot::new(),
        db.clone(),
        &bus,
        Arc::new(Recorder(tx)),
        idle.clone(),
        Arc::new(test_keys()),
        "Test PC".to_owned(),
        timing,
        shutdown.clone(),
    )
    .await
    .unwrap();
    Harness {
        service,
        seen,
        bus,
        idle,
        db,
        shutdown,
    }
}

async fn harness() -> Harness {
    harness_with(timing()).await
}

impl Harness {
    /// Waits for a connection state (skipping others).
    async fn state(&mut self, want: SocialConnectionState) -> SocialConnection {
        loop {
            let seen = tokio::time::timeout(Duration::from_secs(5), self.seen.recv())
                .await
                .unwrap_or_else(|_| panic!("no {want:?} in time"))
                .unwrap();
            if let Seen::Connection(c) = seen
                && c.state == want
            {
                return c;
            }
        }
    }

    async fn next(&mut self, pick: impl Fn(&Seen) -> bool) -> Seen {
        loop {
            let seen = tokio::time::timeout(Duration::from_secs(5), self.seen.recv())
                .await
                .expect("an event in time")
                .unwrap();
            if pick(&seen) {
                return seen;
            }
        }
    }

    async fn add_server_row(&self, id: Uuid, url: &str) {
        let url = url.to_owned();
        self.db
            .call(move |c| {
                c.execute(
                    "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at) VALUES (?1, ?2, 'Test', ?3, 'VG1-TEST', 0)",
                    rusqlite::params![id.to_string(), url, vec![7u8; 32]],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }
}

fn friend_json(id: u128, name: &str) -> Value {
    json!({"user": {"id": Uuid::from_u128(id), "username": name}, "state": "accepted",
           "presence": {"status": "offline"}})
}

// ---- tests ----------------------------------------------------------------------------------

#[tokio::test]
async fn connects_resyncs_and_follows_events() {
    let mut server = Server::start("token-a", 0).await;
    *server.gw.friends.lock().unwrap() =
        json!({"friends": [friend_json(5, "ana")], "incoming": [], "outgoing": []});
    let mut h = harness().await;
    assert_eq!(
        h.service.connection().state,
        SocialConnectionState::SignedOut
    );
    assert_eq!(
        h.service.friends_list().await,
        Err(SocialError::NotSignedIn)
    );

    let server_id = Uuid::from_u128(100);
    h.service.sessions().set(Some(server.session(server_id)));
    let c = h.state(SocialConnectionState::Connected).await;
    assert_eq!(c.server_id, Some(server_id));
    // hello → REST resync and presence.
    let Seen::Friends(list) = h.next(|s| matches!(s, Seen::Friends(_))).await else {
        unreachable!()
    };
    assert_eq!(list.friends[0].user.username, "ana");
    assert_eq!(server.next_presence().await, json!({"status": "online"}));

    // presence.changed updates the cache and the UI.
    server.push(
        "presence.changed",
        json!({"user_id": Uuid::from_u128(5), "status": "in_game", "package_title": "Coop"}),
    );
    let Seen::Presence(user, p) = h.next(|s| matches!(s, Seen::Presence(..))).await else {
        unreachable!()
    };
    assert_eq!(
        (user, p.status, p.package_title.as_deref()),
        (Uuid::from_u128(5), PresenceStatus::InGame, Some("Coop"))
    );

    // friend.request → resync, then a toast for the new incoming request.
    *server.gw.friends.lock().unwrap() = json!({"friends": [friend_json(5, "ana")],
        "incoming": [{"user": {"id": Uuid::from_u128(9), "username": "bo"}, "state": "incoming"}], "outgoing": []});
    server.push("friend.request", json!({"user_id": Uuid::from_u128(9)}));
    let Seen::Request(u) = h.next(|s| matches!(s, Seen::Request(_))).await else {
        unreachable!()
    };
    assert_eq!(u.username, "bo");
    // Other kinds are ignored here (messaging and invites come later).
    server.push(
        "inbox.new",
        json!({"conversation_id": Uuid::nil(), "count": 1}),
    );
    h.shutdown.cancel();
}

#[tokio::test]
async fn reconnects_after_drops_restarts_and_network_changes() {
    let mut server = Server::start("token-a", 0).await;
    let mut h = harness().await;
    h.service
        .sessions()
        .set(Some(server.session(Uuid::from_u128(100))));
    h.state(SocialConnectionState::Connected).await;
    server.next_presence().await;

    // The server drops the socket: reconnect, and presence is sent again after hello.
    server.gw.control.send(Control::Drop).unwrap();
    h.state(SocialConnectionState::Reconnecting).await;
    h.state(SocialConnectionState::Connected).await;
    assert_eq!(server.next_presence().await, json!({"status": "online"}));
    assert_eq!(server.gw.connections.load(Ordering::SeqCst), 2);

    // The server goes away and comes back on the same port.
    server.stop().await;
    h.state(SocialConnectionState::Reconnecting).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    server.restart().await;
    h.state(SocialConnectionState::Connected).await;
    server.next_presence().await;

    // A restart close (1012) also reconnects.
    server.gw.control.send(Control::Close(1012)).unwrap();
    h.state(SocialConnectionState::Connected).await;
    assert!(server.gw.connections.load(Ordering::SeqCst) >= 4);
    h.shutdown.cancel();
}

#[tokio::test]
async fn a_network_change_skips_the_backoff() {
    let server = Server::start("token-a", 0).await;
    let mut h = harness_with(Timing {
        min_backoff: Duration::from_secs(30),
        max_backoff: Duration::from_secs(60),
        ..timing()
    })
    .await;
    h.service
        .sessions()
        .set(Some(server.session(Uuid::from_u128(100))));
    h.state(SocialConnectionState::Connected).await;
    server.gw.control.send(Control::Drop).unwrap();
    let waiting = h.state(SocialConnectionState::Reconnecting).await;
    let waiting = if waiting.retry_at.is_some() {
        waiting
    } else {
        h.state(SocialConnectionState::Reconnecting).await
    };
    assert!(
        waiting.retry_at.is_some(),
        "the UI sees when the next attempt is"
    );
    let started = std::time::Instant::now();
    h.service.network_changed();
    h.state(SocialConnectionState::Connected).await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "reconnected without the 30 s backoff"
    );
    h.shutdown.cancel();
}

#[tokio::test]
async fn a_silent_socket_is_replaced() {
    let server = Server::start("token-a", 0).await;
    let mut h = harness_with(Timing {
        dead_after: Duration::from_millis(300),
        ..timing()
    })
    .await;
    h.service
        .sessions()
        .set(Some(server.session(Uuid::from_u128(100))));
    h.state(SocialConnectionState::Connected).await;
    server.gw.control.send(Control::Silence).unwrap();
    h.state(SocialConnectionState::Reconnecting).await;
    h.state(SocialConnectionState::Connected).await;
    assert!(server.gw.connections.load(Ordering::SeqCst) >= 2);
    h.shutdown.cancel();
}

#[tokio::test]
async fn switching_servers_signing_out_and_revoked_sessions() {
    let a = Server::start("token-a", 0).await;
    let b = Server::start("token-b", 0).await;
    let mut h = harness().await;
    let unauthorized = Arc::new(Mutex::new(Vec::new()));
    let seen = unauthorized.clone();
    h.service
        .sessions()
        .on_unauthorized(move |id| seen.lock().unwrap().push(id));

    h.service
        .sessions()
        .set(Some(a.session(Uuid::from_u128(1))));
    h.state(SocialConnectionState::Connected).await;
    // A refreshed token for the same server keeps the socket.
    let mut refreshed = a.session(Uuid::from_u128(1));
    refreshed.access_token = Arc::new(Zeroizing::new("token-a".into()));
    h.service.sessions().set(Some(refreshed));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(a.gw.connections.load(Ordering::SeqCst), 1);

    // Switching to server B closes A and connects to B.
    h.service
        .sessions()
        .set(Some(b.session(Uuid::from_u128(2))));
    let c = h.state(SocialConnectionState::Connected).await;
    assert_eq!(c.server_id, Some(Uuid::from_u128(2)));
    assert_eq!(b.gw.connections.load(Ordering::SeqCst), 1);

    // Signing out closes the socket.
    h.service.sessions().set(None);
    h.state(SocialConnectionState::SignedOut).await;

    // A revoked session (4001) reports the token and does not reconnect by itself.
    h.service
        .sessions()
        .set(Some(b.session(Uuid::from_u128(2))));
    h.state(SocialConnectionState::Connected).await;
    b.gw.control.send(Control::Close(4001)).unwrap();
    h.state(SocialConnectionState::Reconnecting).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(b.gw.connections.load(Ordering::SeqCst), 2);
    assert_eq!(
        unauthorized.lock().unwrap().as_slice(),
        [Uuid::from_u128(2)]
    );
    // The `session.revoked` event that precedes the close is enough, even when the close
    // frame itself is lost to a connection reset.
    h.service.sessions().set(None);
    h.state(SocialConnectionState::SignedOut).await;
    h.service
        .sessions()
        .set(Some(b.session(Uuid::from_u128(2))));
    h.state(SocialConnectionState::Connected).await;
    b.gw.control
        .send(Control::Event(
            "session.revoked".into(),
            json!({"reason": "device_revoked"}),
        ))
        .unwrap();
    b.gw.control.send(Control::Drop).unwrap();
    h.state(SocialConnectionState::Reconnecting).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(b.gw.connections.load(Ordering::SeqCst), 3);
    assert_eq!(
        unauthorized.lock().unwrap().as_slice(),
        [Uuid::from_u128(2), Uuid::from_u128(2)]
    );
    // A wrong token is refused at the ticket and reported too.
    let mut wrong = b.session(Uuid::from_u128(3));
    wrong.access_token = Arc::new(Zeroizing::new("nope".into()));
    h.service.sessions().set(Some(wrong));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(unauthorized.lock().unwrap().contains(&Uuid::from_u128(3)));
    h.shutdown.cancel();
}

#[tokio::test]
async fn presence_follows_games_idle_time_and_privacy() {
    let mut server = Server::start("token-a", 0).await;
    let mut h = harness().await;
    let server_id = Uuid::from_u128(100);
    h.service.sessions().set(Some(server.session(server_id)));
    h.state(SocialConnectionState::Connected).await;
    assert_eq!(server.next_presence().await, json!({"status": "online"}));

    let game = PackageRef {
        server_id,
        package_id: Uuid::from_u128(42),
    };
    h.bus.publish(AppEvent::GameStarted(GameStarted {
        package: game,
        pid: 1,
    }));
    assert_eq!(
        server.next_presence().await,
        json!({"status": "in_game", "package_id": Uuid::from_u128(42)})
    );
    // Idle polls while nothing changes send nothing.
    server.no_presence_for(Duration::from_millis(300)).await;

    // Hiding the game is stored and applied at once.
    let mut settings = h.service.settings_get().await.unwrap();
    assert!(settings.show_current_game);
    settings.show_current_game = false;
    h.service.settings_set(settings).await.unwrap();
    assert_eq!(server.next_presence().await, json!({"status": "in_game"}));
    assert!(!h.service.settings_get().await.unwrap().show_current_game);

    h.bus.publish(AppEvent::GameStopped(GameStopped {
        package: game,
        exit: GameExit {
            code: Some(0),
            stopped_by_user: false,
            session_seconds: 60,
        },
    }));
    assert_eq!(server.next_presence().await, json!({"status": "online"}));
    *h.idle.0.lock().unwrap() = Some(Duration::from_secs(11 * 60));
    assert_eq!(server.next_presence().await, json!({"status": "away"}));
    *h.idle.0.lock().unwrap() = Some(Duration::from_secs(1));
    assert_eq!(server.next_presence().await, json!({"status": "online"}));

    // Invalid settings are refused.
    let bad = SocialSettings {
        overlay_hotkey: " ".into(),
        ..SocialSettings::default()
    };
    assert!(matches!(
        h.service.settings_set(bad).await,
        Err(SocialError::InvalidInput { .. })
    ));
    h.shutdown.cancel();
}

#[tokio::test]
async fn friend_actions_and_local_blocks() {
    let server = Server::start("token-a", 0).await;
    let h = harness().await;
    let server_id = Uuid::from_u128(100);
    h.add_server_row(server_id, &format!("http://{}/", server.addr))
        .await;
    h.service.sessions().set(Some(server.session(server_id)));

    // Friend codes are normalized before they leave the launcher; nonsense never does.
    h.service
        .friend_request_send(FriendTarget::Code {
            code: "abcd-efgh".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        server.gw.requests.lock().unwrap()[0],
        json!({"friend_code": "ABCDEFGH"})
    );
    let err = h
        .service
        .friend_request_send(FriendTarget::Code { code: "no!".into() })
        .await
        .unwrap_err();
    assert!(matches!(err, SocialError::InvalidInput { ref field, .. } if field == "code"));
    assert_eq!(server.gw.requests.lock().unwrap().len(), 1);

    // Blocks are kept locally (the server has no list).
    *server.gw.friends.lock().unwrap() =
        json!({"friends": [friend_json(5, "ana")], "incoming": [], "outgoing": []});
    h.service.friends_list().await.unwrap();
    h.service.user_block(Uuid::from_u128(5)).await.unwrap();
    let blocks = h.service.blocks_list().await.unwrap();
    assert_eq!(
        (blocks.len(), blocks[0].username.as_deref()),
        (1, Some("ana"))
    );
    h.service.user_unblock(Uuid::from_u128(5)).await.unwrap();
    assert!(h.service.blocks_list().await.unwrap().is_empty());

    // Unreachable servers are `offline`.
    server.stop().await;
    assert_eq!(
        h.service.friend_code_create().await,
        Err(SocialError::Offline)
    );
    h.shutdown.cancel();
}
