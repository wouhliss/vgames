//! Realtime WebSocket gateway (docs/architecture/03-api.md §6, A1-T05).
//!
//! REST stays the source of truth; the socket only nudges clients and carries ephemeral
//! presence. Other modules publish with [`bus::publish`] and handle client events by
//! registering an [`hub::InboundHandler`] (see `crate::social::realtime_handlers`).

pub mod bus;
pub mod hub;

use std::time::Duration;

use axum::{
    extract::{
        Query, State,
        ws::{
            CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade,
            rejection::WebSocketUpgradeRejection,
        },
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use time::OffsetDateTime;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_proto::realtime::{Envelope, Hello, RealtimeTicket, close};

use crate::{
    auth::{CurrentUser, tokens},
    error::{ApiError, ApiResult},
    openapi_problems::Unauthorized,
    state::AppState,
};
use hub::{InboundContext, Outbound};

pub const TICKET_TTL: time::Duration = time::Duration::seconds(30);
pub const PING_INTERVAL: Duration = Duration::from_secs(25);
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
pub use vgames_proto::realtime::MAX_FRAME;
const HANDLER_TIMEOUT: Duration = Duration::from_secs(10);

/// Inbound text frames one connection may send back to back before [`FRAMES_PER_SECOND`]
/// applies (GAME-14 S5). The launcher only sends `presence.set` and `typing`, a few per
/// minute; heartbeats are WebSocket-level pings and are not counted.
pub const FRAME_BURST: u32 = 20;
/// Sustained inbound text frames per second per connection. Frames over the limit are
/// dropped without a reply, so a flood cannot make the server answer at socket speed.
pub const FRAMES_PER_SECOND: u32 = 10;
/// Frames dropped in a row after which the socket is closed with the standard policy
/// violation code 1008. The client may reconnect, which costs a rate-limited ticket.
pub const DROPS_BEFORE_CLOSE: u32 = 200;
const FLOOD_CLOSE: u16 = 1008;
const FRAME_INTERVAL: Duration = Duration::from_millis(1000 / FRAMES_PER_SECOND as u64);

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_ticket))
        .routes(routes!(connect))
}

/// Starts the `LISTEN` task that feeds this instance's sockets, and the presence monitor
/// that keeps this instance's users online (Agent 4, 05-social-notes §2.4).
pub fn start(state: &AppState) -> tokio::task::JoinHandle<()> {
    crate::social::presence::start(state);
    tokio::spawn(bus::listen(state.clone()))
}

/// One-time WebSocket ticket (30 s)
#[utoipa::path(
    post,
    path = "/v1/realtime/ticket",
    tag = "realtime",
    operation_id = "createRealtimeTicket",
    responses((status = 201, description = "Ticket", body = RealtimeTicket), Unauthorized)
)]
pub async fn create_ticket(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<(StatusCode, axum::Json<RealtimeTicket>)> {
    let ticket = tokens::random_b64(32)?;
    let expires_at = OffsetDateTime::now_utc() + TICKET_TTL;
    sqlx::query!("DELETE FROM realtime_tickets WHERE expires_at < now()")
        .execute(&state.db)
        .await?;
    sqlx::query!(
        "INSERT INTO realtime_tickets (digest, session_id, expires_at) VALUES ($1, $2, $3)",
        tokens::digest(&ticket),
        user.session_id,
        expires_at
    )
    .execute(&state.db)
    .await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(RealtimeTicket { ticket, expires_at }),
    ))
}

#[derive(Deserialize)]
pub struct TicketQuery {
    ticket: Option<String>,
}

struct Peer {
    user_id: Uuid,
    session_id: Uuid,
    device_id: Option<Uuid>,
}

/// WebSocket upgrade (protocol in docs/architecture/03-api.md §6)
#[utoipa::path(
    get,
    path = "/v1/realtime",
    tag = "realtime",
    operation_id = "connectRealtime",
    security(("wsTicket" = [])),
    responses((status = 101, description = "Switching protocols"), Unauthorized)
)]
pub async fn connect(
    State(state): State<AppState>,
    Query(q): Query<TicketQuery>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> ApiResult<Response> {
    let ws = ws.map_err(|_| {
        ApiError::bad_request(
            "websocket_required",
            "This endpoint needs a WebSocket upgrade",
        )
    })?;
    let ticket = q
        .ticket
        .filter(|t| t.len() <= 64)
        .ok_or_else(ApiError::unauthenticated)?;
    let row = sqlx::query!(
        r#"DELETE FROM realtime_tickets t
           USING sessions s, users u
           WHERE t.digest = $1 AND t.expires_at > now() AND s.id = t.session_id AND u.id = s.user_id
           RETURNING s.id AS session_id, s.user_id, s.device_id, s.kind, s.revoked_at,
                     s.access_expires_at, s.refresh_expires_at, u.disabled_at"#,
        tokens::digest(&ticket)
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::unauthenticated)?;
    let now = OffsetDateTime::now_utc();
    let live = if row.kind == "web" {
        row.access_expires_at > now
    } else {
        row.refresh_expires_at.is_some_and(|t| t > now)
    };
    if row.revoked_at.is_some() || row.disabled_at.is_some() || !live {
        return Err(ApiError::unauthenticated());
    }
    let peer = Peer {
        user_id: row.user_id,
        session_id: row.session_id,
        device_id: row.device_id,
    };
    Ok(ws
        .max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| run_socket(state, socket, peer))
        .into_response())
}

fn close_frame(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: Utf8Bytes::from_static(reason),
    }))
}

fn text(env: &Envelope) -> Option<Message> {
    serde_json::to_string(env)
        .ok()
        .map(|s| Message::Text(s.into()))
}

async fn run_socket(state: AppState, socket: WebSocket, peer: Peer) {
    let (mut sink, mut stream) = socket.split();
    let (conn_id, mut rx) = state.realtime.register(peer.user_id, peer.session_id);
    tracing::debug!(user_id = %peer.user_id, "realtime connected");

    let hello = bus::envelope(
        "hello",
        Hello {
            user_id: peer.user_id,
            device_id: peer.device_id,
            server_time: OffsetDateTime::now_utc(),
        },
    );
    if let Some(msg) = hello.ok().as_ref().and_then(text)
        && sink.send(msg).await.is_err()
    {
        state.realtime.unregister(peer.user_id, conn_id);
        return;
    }

    let ctx = InboundContext {
        state: state.clone(),
        user_id: peer.user_id,
        session_id: peer.session_id,
        device_id: peer.device_id,
    };
    let mut ping =
        tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    let mut last_seen = tokio::time::Instant::now();
    let mut budget = FrameBudget::new(last_seen);

    let close: Option<Message> = loop {
        tokio::select! {
            _ = state.shutdown.cancelled() => break Some(close_frame(1012, "server restarting")),
            out = rx.recv() => match out {
                Some(Outbound::Event(json)) => {
                    if sink.send(Message::Text(Utf8Bytes::from(json.as_ref()))).await.is_err() { break None; }
                }
                Some(Outbound::Close(code, reason)) => break Some(close_frame(code, reason)),
                // The hub dropped us for falling behind.
                None => break Some(close_frame(close::TOO_SLOW, "too slow; reconnect and resync")),
            },
            _ = ping.tick() => {
                if last_seen.elapsed() > IDLE_TIMEOUT {
                    break Some(close_frame(1001, "idle timeout"));
                }
                if sink.send(Message::Ping(Default::default())).await.is_err() { break None; }
            }
            incoming = stream.next() => {
                let Some(Ok(msg)) = incoming else { break None };
                last_seen = tokio::time::Instant::now();
                match msg {
                    Message::Text(t) => {
                        let handled = on_text(&mut budget, last_seen, t.as_str(), |env| {
                            dispatch(&state, &ctx, env)
                        })
                        .await;
                        match handled {
                            Ok(None) => {}
                            Ok(Some(reply)) => {
                                if sink.send(reply).await.is_err() { break None; }
                            }
                            Err(()) => {
                                tracing::info!(user_id = %peer.user_id, "realtime frame flood; closing");
                                break Some(close_frame(FLOOD_CLOSE, "too many frames"));
                            }
                        }
                    }
                    Message::Binary(_) => break Some(close_frame(1003, "text frames only")),
                    Message::Close(_) => break None,
                    Message::Ping(_) | Message::Pong(_) => {}
                }
            }
        }
    };
    state.realtime.unregister(peer.user_id, conn_id);
    if let Some(frame) = close {
        let _ = sink.send(frame).await;
    }
    let _ = sink.close().await;
    tracing::debug!(user_id = %peer.user_id, "realtime disconnected");
}

/// Per-connection token bucket for inbound text frames ([`FRAME_BURST`],
/// [`FRAMES_PER_SECOND`]). Time is passed in so tests can drive it.
#[derive(Debug)]
struct FrameBudget {
    tokens: u32,
    refilled_at: tokio::time::Instant,
    dropped_in_a_row: u32,
}

#[derive(Debug, PartialEq, Eq)]
enum Admit {
    Handle,
    Drop,
    Close,
}

impl FrameBudget {
    fn new(now: tokio::time::Instant) -> Self {
        Self {
            tokens: FRAME_BURST,
            refilled_at: now,
            dropped_in_a_row: 0,
        }
    }

    fn admit(&mut self, now: tokio::time::Instant) -> Admit {
        let elapsed = now.saturating_duration_since(self.refilled_at);
        let earned = elapsed
            .as_nanos()
            .checked_div(FRAME_INTERVAL.as_nanos())
            .unwrap_or(0);
        let earned = u32::try_from(earned).unwrap_or(u32::MAX);
        if earned > 0 {
            self.tokens = self.tokens.saturating_add(earned).min(FRAME_BURST);
            self.refilled_at = if self.tokens == FRAME_BURST {
                now
            } else {
                // Keep the fractional remainder; `earned < FRAME_BURST` here.
                self.refilled_at + FRAME_INTERVAL.saturating_mul(earned)
            };
        }
        if let Some(left) = self.tokens.checked_sub(1) {
            self.tokens = left;
            self.dropped_in_a_row = 0;
            Admit::Handle
        } else {
            self.dropped_in_a_row = self.dropped_in_a_row.saturating_add(1);
            if self.dropped_in_a_row >= DROPS_BEFORE_CLOSE {
                Admit::Close
            } else {
                Admit::Drop
            }
        }
    }
}

/// Handles one client text frame: charges the connection's [`FrameBudget`] before anything
/// else, answers `ping` locally and hands other events to `dispatch`. Returns an optional
/// reply, or `Err` when the socket should be closed for flooding. Dropped frames get no
/// reply (GAME-14 S5).
async fn on_text<F, Fut>(
    budget: &mut FrameBudget,
    now: tokio::time::Instant,
    raw: &str,
    dispatch: F,
) -> Result<Option<Message>, ()>
where
    F: FnOnce(Envelope) -> Fut,
    Fut: Future<Output = Option<Message>>,
{
    match budget.admit(now) {
        Admit::Handle => {}
        Admit::Drop => return Ok(None),
        Admit::Close => return Err(()),
    }
    let env = match decode_frame(raw.as_bytes()) {
        Ok(e) => e,
        Err(_) => {
            return Ok(error_reply(
                "invalid_frame",
                "frames must be {\"v\":1,\"type\":…,\"data\":…}",
            ));
        }
    };
    Ok(match env.kind.as_str() {
        "ping" => bus::envelope("pong", serde_json::Value::Null)
            .ok()
            .as_ref()
            .and_then(text),
        "pong" => None,
        _ => dispatch(env).await,
    })
}

/// Runs the registered handler for a client event; returns an optional reply.
async fn dispatch(state: &AppState, ctx: &InboundContext, env: Envelope) -> Option<Message> {
    match state.realtime.inbound_handler(env.kind.as_str()) {
        None => error_reply("unknown_event", "unknown event type"),
        Some(h) => match tokio::time::timeout(HANDLER_TIMEOUT, h(ctx.clone(), env.data)).await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => error_reply_owned(e.code.to_string(), e.title.to_string()),
            Err(_) => error_reply("timeout", "the server took too long"),
        },
    }
}

fn error_reply(code: &str, message: &str) -> Option<Message> {
    error_reply_owned(code.to_string(), message.to_string())
}

fn error_reply_owned(code: String, message: String) -> Option<Message> {
    bus::envelope(
        "error",
        serde_json::json!({ "code": code, "message": message }),
    )
    .ok()
    .as_ref()
    .and_then(text)
}

#[derive(Debug, thiserror::Error)]
enum FrameError {
    #[error("realtime frame exceeds the size limit")]
    TooLarge,
    #[error("invalid realtime envelope")]
    Malformed,
    #[error("unsupported realtime protocol version")]
    UnsupportedVersion,
}

/// The byte boundary is shared by the socket handler and adversarial-input tests.
fn decode_frame(raw: &[u8]) -> Result<Envelope, FrameError> {
    if raw.len() > MAX_FRAME {
        return Err(FrameError::TooLarge);
    }
    let frame: Envelope = serde_json::from_slice(raw).map_err(|_| FrameError::Malformed)?;
    if frame.v != 1 {
        return Err(FrameError::UnsupportedVersion);
    }
    Ok(frame)
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const VALID: &[u8] = br#"{"v":1,"type":"ping","data":null}"#;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn arbitrary_realtime_bytes_never_panic(raw in prop::collection::vec(any::<u8>(), 0..MAX_FRAME * 2)) {
            let decoded = decode_frame(&raw);
            if let Ok(frame) = decoded {
                prop_assert_eq!(frame.v, 1);
                prop_assert!(raw.len() <= MAX_FRAME);
            }
        }

        #[test]
        fn mutated_realtime_frames_never_panic(
            mutations in prop::collection::vec((0usize..VALID.len(), any::<u8>()), 0..8),
            keep in 0usize..=VALID.len(),
        ) {
            let mut raw = VALID.to_vec();
            for (at, byte) in mutations {
                raw[at] = byte;
            }
            raw.truncate(keep);
            let _ = decode_frame(&raw);
        }

        #[test]
        fn oversized_valid_realtime_frames_are_rejected(extra in 1usize..4096) {
            let mut raw = VALID.to_vec();
            raw.resize(MAX_FRAME + extra, b' ');
            prop_assert!(decode_frame(&raw).is_err());
        }
    }

    const PING: &str = r#"{"v":1,"type":"ping","data":null}"#;
    const UNKNOWN: &str = r#"{"v":1,"type":"nope","data":{}}"#;

    /// Stands in for `dispatch` with no handler registered for the event.
    async fn unknown(_env: Envelope) -> Option<Message> {
        error_reply("unknown_event", "unknown event type")
    }

    fn reply_kind(reply: &Message) -> String {
        let Message::Text(t) = reply else {
            panic!("expected a text reply, got {reply:?}")
        };
        let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
        v["type"].as_str().unwrap().to_string()
    }

    /// Sends `frames` at one instant; returns the replies, or `None` if the socket closed.
    async fn flood(
        budget: &mut FrameBudget,
        now: tokio::time::Instant,
        raw: &str,
        frames: u32,
    ) -> Option<Vec<Message>> {
        let mut replies = Vec::new();
        for _ in 0..frames {
            match on_text(budget, now, raw, unknown).await {
                Ok(Some(r)) => replies.push(r),
                Ok(None) => {}
                Err(()) => return None,
            }
        }
        Some(replies)
    }

    #[tokio::test]
    async fn a_ping_flood_gets_at_most_a_burst_of_pongs() {
        let now = tokio::time::Instant::now();
        let mut budget = FrameBudget::new(now);
        let replies = flood(&mut budget, now, PING, FRAME_BURST + DROPS_BEFORE_CLOSE - 1)
            .await
            .expect("not closed yet");
        assert_eq!(replies.len(), FRAME_BURST as usize);
        assert!(replies.iter().all(|r| reply_kind(r) == "pong"));
    }

    #[tokio::test]
    async fn unknown_and_invalid_floods_get_at_most_a_burst_of_errors() {
        for raw in [UNKNOWN, "garbage"] {
            let now = tokio::time::Instant::now();
            let mut budget = FrameBudget::new(now);
            let mut dispatched = 0u32;
            let mut replies = 0u32;
            for _ in 0..FRAME_BURST * 5 {
                let reply = on_text(&mut budget, now, raw, |env| {
                    dispatched += 1;
                    unknown(env)
                })
                .await
                .expect("not closed yet");
                if let Some(r) = reply {
                    assert_eq!(reply_kind(&r), "error");
                    replies += 1;
                }
            }
            assert_eq!(replies, FRAME_BURST, "{raw}");
            assert!(dispatched <= FRAME_BURST, "{raw}");
        }
    }

    #[tokio::test]
    async fn a_sustained_flood_closes_the_socket() {
        let now = tokio::time::Instant::now();
        let mut budget = FrameBudget::new(now);
        assert!(
            flood(&mut budget, now, PING, FRAME_BURST + DROPS_BEFORE_CLOSE)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn normal_traffic_keeps_getting_replies() {
        let start = tokio::time::Instant::now();
        let mut budget = FrameBudget::new(start);
        // A steady stream at the sustained rate is never throttled.
        for i in 0..(FRAMES_PER_SECOND * 60) {
            let now = start + FRAME_INTERVAL * i;
            let reply = on_text(&mut budget, now, PING, unknown).await.unwrap();
            assert_eq!(
                reply.as_ref().map(reply_kind).as_deref(),
                Some("pong"),
                "{i}"
            );
        }
        // After a burst is spent, the bucket refills with time.
        let later = start + Duration::from_secs(120);
        let spent = flood(&mut budget, later, PING, FRAME_BURST + 5)
            .await
            .unwrap();
        assert_eq!(spent.len(), FRAME_BURST as usize);
        let refilled = later + Duration::from_secs(1);
        let again = flood(&mut budget, refilled, PING, FRAME_BURST)
            .await
            .unwrap();
        assert_eq!(again.len(), FRAMES_PER_SECOND as usize);
        // Handled events (typing, presence.set) still reach their handler and reply nothing.
        let mut reached = false;
        let reply = on_text(
            &mut budget,
            refilled + Duration::from_secs(5),
            UNKNOWN,
            |_| {
                reached = true;
                std::future::ready(None)
            },
        )
        .await
        .unwrap();
        assert!(reached && reply.is_none());
    }

    #[test]
    fn realtime_decoder_enforces_version_and_exact_limit() {
        assert!(decode_frame(VALID).is_ok());
        assert!(decode_frame(br#"{"v":2,"type":"ping"}"#).is_err());
        let mut maximum = VALID.to_vec();
        maximum.resize(MAX_FRAME, b' ');
        assert!(decode_frame(&maximum).is_ok());
        maximum.push(b' ');
        assert!(decode_frame(&maximum).is_err());
    }
}
