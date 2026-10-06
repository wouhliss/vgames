//! A4-T08: launchers chatting through an in-memory relay that follows the API's rules
//! (device registration and binding, key claims, addressable devices, idempotent sends,
//! `unknown_devices`, per-device inboxes, revocation, `inbox.new` / `device.*` events) over
//! real sockets. Signatures and keys are stored and echoed as uploaded, so the launcher's
//! own checks (key signatures, pins) run for real.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_proto::social::{
    ConversationCreate, DeviceRegister, InboxAck, OneTimeKeysUpload, SendMessageRequest,
    SignedOneTimeKey,
};
use zeroize::Zeroizing;

use crate::db::Db;
use crate::events::EventBus;
use crate::social::crypto::SecretKey32;
use crate::social::model::{
    Conversation, DeviceNotice, Message, MessageBody, MessageStatus, SocialConnection, SocialError,
};
use crate::social::ports::{IdleSource, ServerSession, SessionSlot};
use crate::social::realtime::Timing;
use crate::social::service::{SocialEvents, SocialService};
use crate::social::store::Keys;

const SERVER: Uuid = Uuid::from_u128(0x5e);
const ALICE: Uuid = Uuid::from_u128(0xa1);
const BOB: Uuid = Uuid::from_u128(0xb0);

// ---- the relay ------------------------------------------------------------------------------

struct Dev {
    id: Uuid,
    user: Uuid,
    reg: DeviceRegister,
    revoked: bool,
    otks: Vec<SignedOneTimeKey>,
    fallback: Option<SignedOneTimeKey>,
}

struct Env {
    id: Uuid,
    conversation: Uuid,
    sender_user: Uuid,
    sender_device: Uuid,
    recipient: Uuid,
    cmid: Uuid,
    message_type: u8,
    ciphertext: String,
}

#[derive(Default)]
struct RelayState {
    /// token → user (removed when the session is revoked).
    tokens: HashMap<String, Uuid>,
    /// token → the device the session is bound to.
    bound: HashMap<String, Uuid>,
    devices: Vec<Dev>,
    conversations: Vec<(Uuid, Vec<Uuid>)>,
    /// Left out of `GET /conversations` until an inbox page serves a message from them.
    unlisted: HashSet<Uuid>,
    envelopes: Vec<Env>,
    tickets: HashMap<String, String>,
}

struct Relay {
    st: Mutex<RelayState>,
    /// (user, kind, data) pushed to that user's sockets.
    events: broadcast::Sender<(Uuid, String, Value)>,
    /// Sends answer 503 while set.
    down: AtomicBool,
    /// New conversations are unlisted while set, as if created between two syncs.
    unlist_new: AtomicBool,
}

type R = Arc<Relay>;

fn problem(status: StatusCode, code: &str) -> Response {
    (status, axum::Json(json!({"code": code, "title": code}))).into_response()
}

fn caller(r: &Relay, h: &HeaderMap) -> Result<(String, Uuid), Box<Response>> {
    let token = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned)
        .unwrap_or_default();
    match r.st.lock().unwrap().tokens.get(&token) {
        Some(user) => Ok((token, *user)),
        None => Err(Box::new(problem(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
        ))),
    }
}

fn user_json(id: Uuid) -> Value {
    let name = if id == ALICE { "alice" } else { "bob" };
    json!({"id": id, "username": name})
}

fn now_rfc3339() -> String {
    crate::social::model::unix_rfc3339(OffsetDateTime::now_utc().unix_timestamp())
}

impl Relay {
    fn push(&self, users: impl IntoIterator<Item = Uuid>, kind: &str, data: &Value) {
        for u in users {
            let _ = self.events.send((u, kind.to_owned(), data.clone()));
        }
    }

    fn conversation_json(&self, id: Uuid, members: &[Uuid]) -> Value {
        json!({"id": id, "kind": "direct", "members": members.iter().map(|m| user_json(*m)).collect::<Vec<_>>(),
               "created_at": "2026-09-26T10:00:00Z"})
    }
}

async fn ticket(State(r): State<R>, h: HeaderMap) -> Response {
    let (token, _) = match caller(&r, &h) {
        Ok(c) => c,
        Err(e) => return *e,
    };
    let t = Uuid::now_v7().to_string();
    r.st.lock().unwrap().tickets.insert(t.clone(), token);
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
    State(r): State<R>,
    Query(q): Query<TicketQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let user = {
        let mut st = r.st.lock().unwrap();
        let token = st.tickets.remove(&q.ticket);
        token.and_then(|t| st.tokens.get(&t).copied())
    };
    match user {
        Some(user) => ws.on_upgrade(move |s| run_socket(r, user, s)),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

async fn run_socket(r: R, user: Uuid, mut socket: WebSocket) {
    let mut events = r.events.subscribe();
    let hello = json!({"v": 1, "type": "hello", "data": {"user_id": user, "server_time": "2026-09-26T10:00:00Z"}});
    if socket
        .send(WsMessage::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            e = events.recv() => match e {
                Ok((to, kind, data)) if to == user => {
                    if kind == "close" {
                        return;
                    }
                    let frame = json!({"v": 1, "type": kind, "data": data});
                    if socket.send(WsMessage::Text(frame.to_string().into())).await.is_err() { return; }
                }
                Ok(_) => {}
                Err(_) => return,
            },
            m = socket.recv() => match m {
                Some(Ok(WsMessage::Text(t))) => {
                    let f: Value = serde_json::from_str(t.as_str()).unwrap();
                    if f["type"] == "typing" {
                        let conversation: Uuid = serde_json::from_value(f["data"]["conversation_id"].clone()).unwrap();
                        let members = r.st.lock().unwrap().conversations.iter()
                            .find(|c| c.0 == conversation).map(|c| c.1.clone()).unwrap_or_default();
                        if members.contains(&user) {
                            r.push(members.into_iter().filter(|m| *m != user), "typing",
                                &json!({"conversation_id": conversation, "user_id": user}));
                        }
                    }
                }
                Some(Ok(_)) => {}
                _ => return,
            },
        }
    }
}

async fn friends() -> axum::Json<Value> {
    axum::Json(json!({"friends": [], "incoming": [], "outgoing": []}))
}

async fn register(
    State(r): State<R>,
    h: HeaderMap,
    axum::Json(body): axum::Json<DeviceRegister>,
) -> Response {
    let (token, user) = match caller(&r, &h) {
        Ok(c) => c,
        Err(e) => return *e,
    };
    let mut st = r.st.lock().unwrap();
    let holder = st
        .devices
        .iter()
        .find(|d| d.reg.identity_key == body.identity_key || d.reg.signing_key == body.signing_key)
        .map(|d| (d.id, d.user, d.revoked));
    let (id, status, added) = match holder {
        Some((id, owner, false)) if owner == user => (id, StatusCode::OK, false),
        Some(_) => return problem(StatusCode::CONFLICT, "device_keys_in_use"),
        None => {
            let id = Uuid::now_v7();
            st.devices.push(Dev {
                id,
                user,
                reg: body.clone(),
                revoked: false,
                otks: Vec::new(),
                fallback: None,
            });
            (id, StatusCode::CREATED, true)
        }
    };
    st.bound.insert(token, id);
    drop(st);
    if added {
        // Contacts (everyone here) and the user's other devices hear about it.
        r.push(
            [ALICE, BOB],
            "device.added",
            &json!({"user_id": user, "device_id": id}),
        );
    }
    (
        status,
        axum::Json(
            json!({"id": id, "display_name": body.display_name, "platform": body.platform,
                          "created_at": now_rfc3339()}),
        ),
    )
        .into_response()
}

async fn my_devices(State(r): State<R>, h: HeaderMap) -> Response {
    let (token, user) = match caller(&r, &h) {
        Ok(c) => c,
        Err(e) => return *e,
    };
    let st = r.st.lock().unwrap();
    let current = st.bound.get(&token).copied();
    let items: Vec<Value> = st
        .devices
        .iter()
        .filter(|d| d.user == user && !d.revoked)
        .map(|d| {
            json!({"id": d.id, "display_name": d.reg.display_name, "platform": d.reg.platform,
                   "identity_key": d.reg.identity_key, "signing_key": d.reg.signing_key,
                   "one_time_keys_available": d.otks.len(), "has_fallback_key": d.fallback.is_some(),
                   "current": Some(d.id) == current, "created_at": now_rfc3339()})
        })
        .collect();
    axum::Json(json!({ "items": items })).into_response()
}

async fn revoke(State(r): State<R>, h: HeaderMap, Path(id): Path<Uuid>) -> Response {
    let (_, user) = match caller(&r, &h) {
        Ok(c) => c,
        Err(e) => return *e,
    };
    let mut st = r.st.lock().unwrap();
    let Some(d) = st
        .devices
        .iter_mut()
        .find(|d| d.id == id && d.user == user && !d.revoked)
    else {
        return problem(StatusCode::NOT_FOUND, "not_found");
    };
    d.revoked = true;
    d.otks.clear();
    d.fallback = None;
    // The device's sessions are revoked too.
    let gone: Vec<String> = st
        .bound
        .iter()
        .filter(|(_, d)| **d == id)
        .map(|(t, _)| t.clone())
        .collect();
    for t in gone {
        st.tokens.remove(&t);
    }
    drop(st);
    r.push(
        [ALICE, BOB],
        "device.revoked",
        &json!({"user_id": user, "device_id": id}),
    );
    StatusCode::NO_CONTENT.into_response()
}

async fn upload(
    State(r): State<R>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    axum::Json(body): axum::Json<OneTimeKeysUpload>,
) -> Response {
    let (token, _) = match caller(&r, &h) {
        Ok(c) => c,
        Err(e) => return *e,
    };
    let mut st = r.st.lock().unwrap();
    if st.bound.get(&token) != Some(&id) {
        return problem(StatusCode::FORBIDDEN, "not_your_device");
    }
    let d = st.devices.iter_mut().find(|d| d.id == id).unwrap();
    d.otks.extend(body.one_time_keys);
    if body.fallback_key.is_some() {
        d.fallback = body.fallback_key;
    }
    axum::Json(json!({"available": d.otks.len()})).into_response()
}

async fn user_devices(State(r): State<R>, h: HeaderMap, Path(user): Path<Uuid>) -> Response {
    if let Err(e) = caller(&r, &h) {
        return *e;
    }
    let st = r.st.lock().unwrap();
    let items: Vec<Value> = st
        .devices
        .iter()
        .filter(|d| d.user == user && !d.revoked)
        .map(|d| {
            json!({"device_id": d.id, "display_name": d.reg.display_name, "identity_key": d.reg.identity_key,
                   "signing_key": d.reg.signing_key, "keys_signature": d.reg.keys_signature,
                   "created_at": now_rfc3339()})
        })
        .collect();
    axum::Json(json!({ "items": items })).into_response()
}

async fn claim(State(r): State<R>, h: HeaderMap, axum::Json(body): axum::Json<Value>) -> Response {
    if let Err(e) = caller(&r, &h) {
        return *e;
    }
    let ids: Vec<Uuid> = serde_json::from_value(body["device_ids"].clone()).unwrap();
    let mut st = r.st.lock().unwrap();
    let mut items = Vec::new();
    for id in ids {
        let Some(d) = st.devices.iter_mut().find(|d| d.id == id && !d.revoked) else {
            continue;
        };
        let (key, is_fallback) = match d.otks.pop() {
            Some(k) => (k, false),
            None => match d.fallback.clone() {
                Some(k) => (k, true),
                None => continue,
            },
        };
        items.push(
            json!({"device_id": id, "key_id": key.key_id, "public_key": key.public_key,
                          "signature": key.signature, "is_fallback": is_fallback}),
        );
    }
    axum::Json(json!({ "items": items })).into_response()
}

async fn conversations(State(r): State<R>, h: HeaderMap) -> Response {
    let (_, user) = match caller(&r, &h) {
        Ok(c) => c,
        Err(e) => return *e,
    };
    let st = r.st.lock().unwrap();
    let items: Vec<Value> = st
        .conversations
        .iter()
        .filter(|c| c.1.contains(&user) && !st.unlisted.contains(&c.0))
        .map(|c| r.conversation_json(c.0, &c.1))
        .collect();
    axum::Json(json!({ "items": items })).into_response()
}

async fn create_conversation(
    State(r): State<R>,
    h: HeaderMap,
    axum::Json(body): axum::Json<ConversationCreate>,
) -> Response {
    let (_, user) = match caller(&r, &h) {
        Ok(c) => c,
        Err(e) => return *e,
    };
    let ConversationCreate::Direct { user_id } = body else {
        return problem(StatusCode::BAD_REQUEST, "unsupported");
    };
    let mut st = r.st.lock().unwrap();
    let existing = st
        .conversations
        .iter()
        .find(|c| c.1.contains(&user) && c.1.contains(&user_id))
        .map(|c| c.0);
    let (id, status) = match existing {
        Some(id) => (id, StatusCode::OK),
        None => {
            let id = Uuid::now_v7();
            st.conversations.push((id, vec![user, user_id]));
            if r.unlist_new.load(Ordering::SeqCst) {
                st.unlisted.insert(id);
            }
            (id, StatusCode::CREATED)
        }
    };
    (
        status,
        axum::Json(r.conversation_json(id, &[user, user_id])),
    )
        .into_response()
}

async fn send(
    State(r): State<R>,
    h: HeaderMap,
    Path(conversation): Path<Uuid>,
    axum::Json(body): axum::Json<SendMessageRequest>,
) -> Response {
    if r.down.load(Ordering::SeqCst) {
        return problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
    }
    let (token, user) = match caller(&r, &h) {
        Ok(c) => c,
        Err(e) => return *e,
    };
    let mut st = r.st.lock().unwrap();
    let Some(&sender) = st.bound.get(&token) else {
        return problem(StatusCode::FORBIDDEN, "device_required");
    };
    let Some(members) = st
        .conversations
        .iter()
        .find(|c| c.0 == conversation)
        .map(|c| c.1.clone())
    else {
        return problem(StatusCode::NOT_FOUND, "not_found");
    };
    if !members.contains(&user) {
        return problem(StatusCode::NOT_FOUND, "not_found");
    }
    let addressable: HashMap<Uuid, Uuid> = st
        .devices
        .iter()
        .filter(|d| members.contains(&d.user) && !d.revoked && d.id != sender)
        .map(|d| (d.id, d.user))
        .collect();
    if body
        .envelopes
        .iter()
        .any(|e| !addressable.contains_key(&e.recipient_device_id))
    {
        return problem(StatusCode::BAD_REQUEST, "unknown_recipient");
    }
    let mut notify = Vec::new();
    for e in body.envelopes {
        let dup = st.envelopes.iter().any(|x| {
            x.sender_device == sender
                && x.cmid == body.client_message_id
                && x.recipient == e.recipient_device_id
        });
        if dup {
            continue;
        }
        notify.push(addressable[&e.recipient_device_id]);
        st.envelopes.push(Env {
            id: Uuid::now_v7(),
            conversation,
            sender_user: user,
            sender_device: sender,
            recipient: e.recipient_device_id,
            cmid: body.client_message_id,
            message_type: e.olm_message_type,
            ciphertext: e.ciphertext,
        });
    }
    // Envelopes already acknowledged are gone from the table, but their devices were served:
    // like the API, remember them through the (never deleted here) served log below.
    let served: Vec<Uuid> = st
        .envelopes
        .iter()
        .filter(|x| x.sender_device == sender && x.cmid == body.client_message_id)
        .map(|x| x.recipient)
        .collect();
    let unknown: Vec<Uuid> = addressable
        .keys()
        .filter(|d| !served.contains(d))
        .copied()
        .collect();
    drop(st);
    notify.sort();
    notify.dedup();
    r.push(
        notify,
        "inbox.new",
        &json!({"conversation_id": conversation, "count": 1}),
    );
    (
        StatusCode::ACCEPTED,
        axum::Json(json!({"accepted": 1, "unknown_devices": unknown})),
    )
        .into_response()
}

async fn inbox(State(r): State<R>, h: HeaderMap) -> Response {
    let (token, _) = match caller(&r, &h) {
        Ok(c) => c,
        Err(e) => return *e,
    };
    let mut st = r.st.lock().unwrap();
    let Some(&device) = st.bound.get(&token) else {
        return problem(StatusCode::FORBIDDEN, "device_required");
    };
    let served: Vec<Uuid> = st
        .envelopes
        .iter()
        .filter(|e| e.recipient == device && e.id != Uuid::nil())
        .map(|e| e.conversation)
        .collect();
    for conversation in served {
        st.unlisted.remove(&conversation);
    }
    let items: Vec<Value> = st
        .envelopes
        .iter()
        .filter(|e| e.recipient == device && e.id != Uuid::nil())
        .map(|e| {
            let key = &st.devices.iter().find(|d| d.id == e.sender_device).unwrap().reg.identity_key;
            json!({"id": e.id, "conversation_id": e.conversation, "sender_user_id": e.sender_user,
                   "sender_device_id": e.sender_device, "sender_identity_key": key, "algorithm": "olm.v1",
                   "olm_message_type": e.message_type, "ciphertext": e.ciphertext, "created_at": now_rfc3339()})
        })
        .collect();
    axum::Json(json!({ "items": items })).into_response()
}

async fn ack(State(r): State<R>, h: HeaderMap, axum::Json(body): axum::Json<InboxAck>) -> Response {
    let (token, _) = match caller(&r, &h) {
        Ok(c) => c,
        Err(e) => return *e,
    };
    let mut st = r.st.lock().unwrap();
    let Some(&device) = st.bound.get(&token) else {
        return problem(StatusCode::FORBIDDEN, "device_required");
    };
    // Acknowledged envelopes stay as tombstones (nil id) so resends still count as served.
    for e in st.envelopes.iter_mut() {
        if e.recipient == device && body.ids.contains(&e.id) {
            e.id = Uuid::nil();
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn start_relay() -> (R, SocketAddr) {
    let r = Arc::new(Relay {
        st: Mutex::new(RelayState::default()),
        events: broadcast::channel(256).0,
        down: AtomicBool::new(false),
        unlist_new: AtomicBool::new(false),
    });
    let app = Router::new()
        .route("/v1/realtime/ticket", post(ticket))
        .route("/v1/realtime", get(socket))
        .route("/v1/friends", get(friends))
        .route("/v1/devices", post(register).get(my_devices))
        .route("/v1/devices/{id}", delete(revoke))
        .route("/v1/devices/{id}/one-time-keys", post(upload))
        .route("/v1/users/{id}/devices", get(user_devices))
        .route("/v1/keys/claim", post(claim))
        .route(
            "/v1/conversations",
            get(conversations).post(create_conversation),
        )
        .route("/v1/conversations/{id}/messages", post(send))
        .route("/v1/inbox", get(inbox))
        .route("/v1/inbox/ack", post(ack))
        .with_state(r.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (r, addr)
}

// ---- launchers ------------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Seen {
    Message(Message),
    Status(Uuid, MessageStatus),
    Notice(DeviceNotice),
    Typing(Uuid, Uuid),
    Conversations(Vec<Uuid>),
}

struct Recorder(mpsc::UnboundedSender<Seen>);

impl SocialEvents for Recorder {
    fn connection_changed(&self, _: &SocialConnection) {}
    fn friends_changed(&self, _: &crate::social::model::FriendList) {}
    fn presence_changed(&self, _: Uuid, _: &crate::social::model::Presence) {}
    fn friend_request_received(&self, _: &crate::social::model::UserSummary) {}
    fn message_received(&self, m: &Message) {
        let _ = self.0.send(Seen::Message(m.clone()));
    }
    fn message_status_changed(&self, id: Uuid, _: Uuid, status: MessageStatus) {
        let _ = self.0.send(Seen::Status(id, status));
    }
    fn device_notice(&self, _: Option<Uuid>, n: &DeviceNotice) {
        let _ = self.0.send(Seen::Notice(n.clone()));
    }
    fn typing(&self, conversation: Uuid, user: Uuid) {
        let _ = self.0.send(Seen::Typing(conversation, user));
    }
    fn conversations_changed(&self, conversations: &[Conversation]) {
        let _ = self.0.send(Seen::Conversations(
            conversations.iter().map(|c| c.id).collect(),
        ));
    }
}

struct NoIdle;
impl IdleSource for NoIdle {
    fn idle_for(&self) -> Option<Duration> {
        None
    }
}

struct Launcher {
    service: SocialService,
    seen: mpsc::UnboundedReceiver<Seen>,
    token: String,
    _shutdown: tokio_util::sync::DropGuard,
}

fn timing() -> Timing {
    Timing {
        min_backoff: Duration::from_millis(50),
        max_backoff: Duration::from_millis(200),
        dead_after: Duration::from_secs(60),
        connect_timeout: Duration::from_secs(5),
        idle_poll: Duration::from_secs(3600),
        network_poll: Duration::from_secs(3600),
    }
}

async fn launcher(r: &R, addr: SocketAddr, user: Uuid) -> Launcher {
    let token = format!("token-{}", Uuid::now_v7());
    r.st.lock().unwrap().tokens.insert(token.clone(), user);
    let db = Db::open_in_memory().unwrap();
    db.call(move |c| {
        c.execute(
            "INSERT INTO servers (id, url, name, root_public_key, root_fingerprint, added_at) VALUES (?1, 'http://x', 'Test', ?2, 'VG1-TEST', 0)",
            rusqlite::params![SERVER.to_string(), vec![7u8; 32]],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let (tx, seen) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let sessions = SessionSlot::new();
    let service = SocialService::start(
        sessions.clone(),
        db,
        &EventBus::new(),
        Arc::new(Recorder(tx)),
        Arc::new(NoIdle),
        timing(),
        shutdown.clone(),
    )
    .await
    .unwrap();
    let keys = Keys::new(
        SecretKey32::generate().unwrap(),
        &SecretKey32::generate().unwrap(),
    )
    .unwrap();
    service.provide_keys(Arc::new(keys));
    sessions.set(Some(ServerSession {
        server_id: SERVER,
        user_id: user,
        base_url: format!("http://{addr}/").parse().unwrap(),
        access_token: Arc::new(Zeroizing::new(token.clone())),
    }));
    Launcher {
        service,
        seen,
        token,
        _shutdown: shutdown.drop_guard(),
    }
}

impl Launcher {
    async fn next(&mut self, what: &str, pick: impl Fn(&Seen) -> bool) -> Seen {
        let wait = async {
            loop {
                let seen = self.seen.recv().await.unwrap();
                if pick(&seen) {
                    return seen;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(15), wait)
            .await
            .unwrap_or_else(|_| panic!("no {what} in time"))
    }

    async fn received(&mut self, text: &str) -> Message {
        let Seen::Message(m) = self
            .next(&format!("message {text:?}"), |s| {
                matches!(s, Seen::Message(m) if m.body == MessageBody::Text { text: text.into() })
            })
            .await
        else {
            unreachable!()
        };
        m
    }

    async fn sent(&mut self, message: Uuid) {
        self.next(
            "sent status",
            |s| matches!(s, Seen::Status(id, MessageStatus::Sent) if *id == message),
        )
        .await;
    }

    /// Nothing matching arrives within `d`.
    async fn quiet(&mut self, d: Duration, pick: impl Fn(&Seen) -> bool) {
        let deadline = tokio::time::Instant::now() + d;
        while let Ok(Some(seen)) = tokio::time::timeout_at(deadline, self.seen.recv()).await {
            assert!(!pick(&seen), "unexpected {seen:?}");
        }
    }

    fn device(&self, r: &R) -> Option<Uuid> {
        r.st.lock().unwrap().bound.get(&self.token).copied()
    }
}

/// Waits until every launcher registered its device and uploaded keys.
async fn registered(r: &R, launchers: &[&Launcher]) {
    for _ in 0..300 {
        let ready = launchers.iter().all(|l| {
            let st = r.st.lock().unwrap();
            st.bound
                .get(&l.token)
                .and_then(|id| st.devices.iter().find(|d| d.id == *id))
                .is_some_and(|d| d.otks.len() >= 20 && d.fallback.is_some())
        });
        if ready {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("devices not registered in time");
}

async fn send_text(from: &Launcher, conversation: Uuid, text: &str) -> Uuid {
    let m = from
        .service
        .message_send(conversation, text.to_owned())
        .await
        .unwrap();
    assert_eq!(m.status, MessageStatus::Pending);
    m.id
}

// ---- tests ----------------------------------------------------------------------------------

#[tokio::test]
async fn two_launchers_chat_and_a_new_device_only_gets_new_messages() {
    let (r, addr) = start_relay().await;
    let mut alice = launcher(&r, addr, ALICE).await;
    let mut bob = launcher(&r, addr, BOB).await;
    registered(&r, &[&alice, &bob]).await;
    // Bob learns of the conversation from its first message, not from a sync.
    r.unlist_new.store(true, Ordering::SeqCst);

    let conversation = alice.service.conversation_open_direct(BOB).await.unwrap();
    let id = send_text(&alice, conversation.id, "hi bob").await;
    alice.sent(id).await;
    // Bob's launcher stores the new conversation before announcing its first message, so
    // a reply sent at once finds it.
    let mut known = false;
    let got = loop {
        match bob
            .next("conversation or message", |s| {
                matches!(s, Seen::Conversations(_) | Seen::Message(_))
            })
            .await
        {
            Seen::Conversations(ids) => known |= ids.contains(&conversation.id),
            Seen::Message(m) => break m,
            _ => {}
        }
    };
    assert!(known, "the conversation is listed before its first message");
    assert_eq!(
        got.body,
        MessageBody::Text {
            text: "hi bob".into()
        }
    );
    assert!(!got.mine);
    assert_eq!(got.sender_user_id, ALICE);
    assert_eq!(got.conversation_id, conversation.id);

    let id = send_text(&bob, conversation.id, "hi alice").await;
    bob.sent(id).await;
    alice.received("hi alice").await;

    // Bob signs in on a third launcher: Alice is told, and it only gets what follows.
    let mut bob2 = launcher(&r, addr, BOB).await;
    registered(&r, &[&bob2]).await;
    let bob2_device = bob2.device(&r).unwrap();
    alice
        .next("new-device notice", |s| {
            matches!(s, Seen::Notice(DeviceNotice::NewDevice { user_id, device_id, .. })
                if *user_id == BOB && *device_id == bob2_device)
        })
        .await;
    let id = send_text(&alice, conversation.id, "welcome").await;
    alice.sent(id).await;
    bob.received("welcome").await;
    bob2.received("welcome").await;
    let history = bob2
        .service
        .messages_list(conversation.id, None, 50)
        .await
        .unwrap();
    let texts: Vec<_> = history
        .iter()
        .filter_map(|m| match &m.body {
            MessageBody::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["welcome"], "old messages never reach a new device");

    // What Bob sends from one device also lands on his other one, as his own message.
    let id = send_text(&bob2, conversation.id, "from my laptop").await;
    bob2.sent(id).await;
    alice.received("from my laptop").await;
    let copy = bob.received("from my laptop").await;
    assert!(copy.mine, "a copy from your own device is yours");

    // Typing reaches the other member only.
    bob.service.typing_start(conversation.id).unwrap();
    alice
        .next(
            "typing",
            |s| matches!(s, Seen::Typing(c, u) if *c == conversation.id && *u == BOB),
        )
        .await;

    // No plaintext ever reached the relay.
    let st = r.st.lock().unwrap();
    for e in &st.envelopes {
        for word in ["hi bob", "hi alice", "welcome", "laptop"] {
            assert!(!e.ciphertext.contains(word));
        }
    }
}

#[tokio::test]
async fn revoking_a_device_stops_delivery_to_it() {
    let (r, addr) = start_relay().await;
    let mut alice = launcher(&r, addr, ALICE).await;
    let mut bob = launcher(&r, addr, BOB).await;
    let mut bob2 = launcher(&r, addr, BOB).await;
    registered(&r, &[&alice, &bob, &bob2]).await;
    let bob2_device = bob2.device(&r).unwrap();

    let conversation = alice.service.conversation_open_direct(BOB).await.unwrap();
    let id = send_text(&alice, conversation.id, "before").await;
    alice.sent(id).await;
    bob.received("before").await;
    bob2.received("before").await;

    // Bob removes his second device from the first one.
    let listed = bob.service.devices_list().await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed.iter().filter(|d| d.current).count(), 1);
    bob.service.device_revoke(bob2_device).await.unwrap();
    alice
        .next("revocation notice", |s| {
            matches!(s, Seen::Notice(DeviceNotice::DeviceRevoked { device_id, .. }) if *device_id == bob2_device)
        })
        .await;

    let id = send_text(&alice, conversation.id, "after").await;
    alice.sent(id).await;
    bob.received("after").await;
    let recipients: Vec<Uuid> =
        r.st.lock()
            .unwrap()
            .envelopes
            .iter()
            .filter(|e| e.sender_user == ALICE)
            .map(|e| e.recipient)
            .collect();
    assert_eq!(
        recipients.iter().filter(|d| **d == bob2_device).count(),
        1,
        "only the message from before went to the revoked device"
    );
    bob2.quiet(
        Duration::from_millis(500),
        |s| matches!(s, Seen::Message(m) if m.body == MessageBody::Text { text: "after".into() }),
    )
    .await;
}

#[tokio::test]
async fn messages_wait_in_the_outbox_while_the_server_is_down() {
    let (r, addr) = start_relay().await;
    let mut alice = launcher(&r, addr, ALICE).await;
    let mut bob = launcher(&r, addr, BOB).await;
    registered(&r, &[&alice, &bob]).await;
    let conversation = alice.service.conversation_open_direct(BOB).await.unwrap();

    r.down.store(true, Ordering::SeqCst);
    let id = send_text(&alice, conversation.id, "eventually").await;
    alice
        .quiet(Duration::from_secs(1), |s| matches!(s, Seen::Status(..)))
        .await;
    let stored = alice
        .service
        .messages_list(conversation.id, None, 10)
        .await
        .unwrap();
    assert_eq!(stored.last().unwrap().status, MessageStatus::Pending);

    // Back up: the retry (after its backoff) delivers it.
    r.down.store(false, Ordering::SeqCst);
    alice.sent(id).await;
    bob.received("eventually").await;
}

#[tokio::test]
async fn a_changed_key_blocks_sending_until_trusted() {
    let (r, addr) = start_relay().await;
    let mut alice = launcher(&r, addr, ALICE).await;
    let mut bob = launcher(&r, addr, BOB).await;
    registered(&r, &[&alice, &bob]).await;
    let bob_device = bob.device(&r).unwrap();
    let conversation = alice.service.conversation_open_direct(BOB).await.unwrap();
    let id = send_text(&alice, conversation.id, "first").await;
    alice.sent(id).await;
    bob.received("first").await;

    // Bob's known device suddenly presents other keys (a reinstall, or a hostile server).
    let mut other = crate::social::crypto::OlmAccount::new();
    other.generate_one_time_keys(5);
    let (other_keys, _) = other.unpublished_keys();
    {
        let forged = other.signed_device_keys(BOB, SERVER);
        let mut st = r.st.lock().unwrap();
        let d = st.devices.iter_mut().find(|d| d.id == bob_device).unwrap();
        d.reg.identity_key = forged.identity_key;
        d.reg.signing_key = forged.signing_key;
        d.reg.keys_signature = forged.keys_signature;
        d.otks = other_keys;
    }
    let security = alice.service.contact_security(BOB).await.unwrap();
    assert!(security.devices.iter().any(|d| d.device_id == bob_device
        && d.state == crate::social::model::ContactDeviceState::KeyChanged));
    let to_bob = |r: &R| {
        r.st.lock()
            .unwrap()
            .envelopes
            .iter()
            .filter(|e| e.sender_user == ALICE && e.recipient == bob_device)
            .count()
    };
    let before = to_bob(&r);
    let id = send_text(&alice, conversation.id, "blocked").await;
    alice
        .next(
            "failed status",
            |s| matches!(s, Seen::Status(m, MessageStatus::Failed) if *m == id),
        )
        .await;
    assert_eq!(
        to_bob(&r),
        before,
        "nothing was encrypted for the changed key"
    );

    // After checking with Bob, Alice accepts the new key: the retry is encrypted for it.
    let security = alice
        .service
        .contact_trust_device(BOB, bob_device)
        .await
        .unwrap();
    assert!(
        security
            .devices
            .iter()
            .all(|d| d.state != crate::social::model::ContactDeviceState::KeyChanged)
    );
    let retried = alice.service.message_retry(id).await.unwrap();
    assert_eq!(retried.status, MessageStatus::Pending);
    alice.sent(id).await;
    assert_eq!(to_bob(&r), before + 1);

    // Unknown conversations and bad input are refused before anything is stored.
    assert_eq!(
        alice.service.message_send(Uuid::nil(), "x".into()).await,
        Err(SocialError::NotFound)
    );
    assert!(matches!(
        alice
            .service
            .message_send(conversation.id, "  ".into())
            .await,
        Err(SocialError::InvalidInput { .. })
    ));
}
