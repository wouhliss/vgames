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
pub const MAX_FRAME: usize = 64 * 1024;
const HANDLER_TIMEOUT: Duration = Duration::from_secs(10);

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
                        if let Some(reply) = handle_text(&state, &ctx, t.as_str()).await
                            && sink.send(reply).await.is_err()
                        {
                            break None;
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

/// Handles one client frame; returns an optional reply.
async fn handle_text(state: &AppState, ctx: &InboundContext, raw: &str) -> Option<Message> {
    let env: Envelope = match serde_json::from_str(raw) {
        Ok(e) => e,
        Err(_) => {
            return error_reply(
                "invalid_frame",
                "frames must be {\"v\":1,\"type\":…,\"data\":…}",
            );
        }
    };
    match env.kind.as_str() {
        "ping" => bus::envelope("pong", serde_json::Value::Null)
            .ok()
            .as_ref()
            .and_then(text),
        "pong" => None,
        kind => match state.realtime.inbound_handler(kind) {
            None => error_reply("unknown_event", "unknown event type"),
            Some(h) => {
                match tokio::time::timeout(HANDLER_TIMEOUT, h(ctx.clone(), env.data)).await {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => error_reply_owned(e.code.to_string(), e.title.to_string()),
                    Err(_) => error_reply("timeout", "the server took too long"),
                }
            }
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
