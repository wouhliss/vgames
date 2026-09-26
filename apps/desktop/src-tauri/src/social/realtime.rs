//! The realtime socket to the active server (03-api §6, 05-social-notes §4).
//!
//! One connection per signed-in session: `POST /v1/realtime/ticket` → `GET /v1/realtime`
//! (WebSocket) → `hello` → the owner resyncs over REST → live events. Rules:
//!
//! - A socket that is silent for [`Timing::dead_after`] (the server pings every 25 s) is
//!   treated as dead and replaced; pings are answered by the WebSocket layer.
//! - Reconnects wait a full-jitter backoff between [`Timing::min_backoff`] and
//!   [`Timing::max_backoff`] (1–60 s), doubling per failed attempt, and reset after a
//!   connection that reached `hello`. A network change ([`RealtimeHandle::network_changed`])
//!   or a new session skips the wait.
//! - A new server or user (not a token refresh) closes the socket and connects again; no
//!   session means `signed_out` and no socket. Close code 4001 (session revoked) reports
//!   the token and waits for the session to change.
//! - Outgoing frames (`presence.set`, `typing`) are dropped while disconnected; their
//!   owners re-send what matters after the next `hello`.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use time::OffsetDateTime;
use tokio::sync::{Notify, mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_proto::realtime::{Envelope, Hello, close, kinds};

use super::api::SocialApi;
use super::model::{SocialConnection, SocialConnectionState, SocialError};
use super::ports::{ServerSession, SessionSlot};

/// Timeouts and backoff (tests shrink them).
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub min_backoff: Duration,
    pub max_backoff: Duration,
    /// No frame at all for this long means the connection is dead.
    pub dead_after: Duration,
    /// Connect + ticket + `hello` must finish within this.
    pub connect_timeout: Duration,
    /// How often OS idle time is read for `away` presence.
    pub idle_poll: Duration,
    /// How often the local route is sampled for network changes (see `netwatch`).
    pub network_poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            min_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            dead_after: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(20),
            idle_poll: super::presence::IDLE_POLL,
            network_poll: Duration::from_secs(10),
        }
    }
}

/// What the connection hands to its owner.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    /// A connection reached `hello`: resync over REST, re-send presence.
    Connected { server_id: Uuid, hello: Hello },
    /// A server event (`presence.changed`, `friend.request`, …).
    Event {
        server_id: Uuid,
        kind: String,
        data: Value,
    },
}

/// Talks to the running connection task.
#[derive(Clone)]
pub struct RealtimeHandle {
    outgoing: mpsc::Sender<(String, Value)>,
    state: watch::Receiver<SocialConnection>,
    network: Arc<Notify>,
}

impl RealtimeHandle {
    /// Queues a client frame; dropped when not connected.
    pub fn send(&self, kind: &str, data: Value) {
        if self.state.borrow().state == SocialConnectionState::Connected {
            let _ = self.outgoing.try_send((kind.to_owned(), data));
        }
    }

    pub fn connection(&self) -> SocialConnection {
        self.state.borrow().clone()
    }

    pub fn watch(&self) -> watch::Receiver<SocialConnection> {
        self.state.clone()
    }

    /// The OS reported a network change: reconnect now instead of waiting out the backoff.
    pub fn network_changed(&self) {
        self.network.notify_one();
    }
}

/// Starts the connection task. `incoming` receives hellos and events in order.
pub fn spawn(
    sessions: SessionSlot,
    http: reqwest::Client,
    timing: Timing,
    incoming: mpsc::Sender<Incoming>,
    shutdown: CancellationToken,
) -> RealtimeHandle {
    let (out_tx, out_rx) = mpsc::channel(64);
    let (state_tx, state_rx) = watch::channel(signed_out());
    let network = Arc::new(Notify::new());
    let task = Task {
        sessions,
        http,
        timing,
        incoming,
        outgoing: out_rx,
        state: state_tx,
        network: network.clone(),
        shutdown,
    };
    tokio::spawn(task.run());
    RealtimeHandle {
        outgoing: out_tx,
        state: state_rx,
        network,
    }
}

fn signed_out() -> SocialConnection {
    SocialConnection {
        server_id: None,
        state: SocialConnectionState::SignedOut,
        retry_at: None,
    }
}

/// Full-jitter backoff: uniform in `[min, min(max, min · 2^attempt)]`.
pub fn backoff(timing: &Timing, attempt: u32) -> Duration {
    let cap = timing
        .min_backoff
        .saturating_mul(1u32.checked_shl(attempt.min(16)).unwrap_or(u32::MAX))
        .min(timing.max_backoff);
    let span = cap.saturating_sub(timing.min_backoff);
    let mut b = [0u8; 8];
    let r = if getrandom::fill(&mut b).is_ok() {
        u64::from_le_bytes(b)
    } else {
        0
    };
    let span_nanos = u64::try_from(span.as_nanos()).unwrap_or(u64::MAX);
    let offset = if span_nanos == 0 { 0 } else { r % span_nanos };
    timing.min_backoff + Duration::from_nanos(offset)
}

/// How one connection ended.
enum Ended {
    /// Reached `hello`, then dropped: reconnect after a short wait.
    AfterHello,
    /// Failed before `hello`.
    Failed,
    /// The session was revoked (4001): wait for a new session.
    Revoked,
    /// The session changed identity or went away: reconnect at once.
    SessionChanged,
    Shutdown,
}

struct Task {
    sessions: SessionSlot,
    http: reqwest::Client,
    timing: Timing,
    incoming: mpsc::Sender<Incoming>,
    outgoing: mpsc::Receiver<(String, Value)>,
    state: watch::Sender<SocialConnection>,
    network: Arc<Notify>,
    shutdown: CancellationToken,
}

impl Task {
    fn set_state(
        &self,
        server_id: Option<Uuid>,
        state: SocialConnectionState,
        retry_at: Option<OffsetDateTime>,
    ) {
        let next = SocialConnection {
            server_id,
            state,
            retry_at: retry_at.and_then(|t| {
                t.format(&time::format_description::well_known::Rfc3339)
                    .ok()
            }),
        };
        self.state.send_if_modified(|cur| {
            if *cur == next {
                false
            } else {
                *cur = next;
                true
            }
        });
    }

    async fn run(mut self) {
        let mut sessions = self.sessions.subscribe();
        let mut attempt: u32 = 0;
        loop {
            let session = sessions.borrow_and_update().clone();
            let Some(session) = session else {
                self.set_state(None, SocialConnectionState::SignedOut, None);
                attempt = 0;
                tokio::select! {
                    () = self.shutdown.cancelled() => return,
                    r = sessions.changed() => if r.is_err() { return },
                }
                continue;
            };
            let state = if attempt == 0 {
                SocialConnectionState::Connecting
            } else {
                SocialConnectionState::Reconnecting
            };
            self.set_state(Some(session.server_id), state, None);
            let ended = self.connect_and_run(&session, &mut sessions).await;
            let wait = match ended {
                Ended::Shutdown => return,
                Ended::SessionChanged => {
                    attempt = 0;
                    continue;
                }
                Ended::Revoked => {
                    self.sessions.report_unauthorized(session.server_id);
                    None
                }
                Ended::AfterHello => {
                    attempt = 1;
                    Some(backoff(&self.timing, 0))
                }
                Ended::Failed => {
                    let d = backoff(&self.timing, attempt);
                    attempt = attempt.saturating_add(1);
                    Some(d)
                }
            };
            let retry_at = wait.map(|d| OffsetDateTime::now_utc() + d);
            self.set_state(
                Some(session.server_id),
                SocialConnectionState::Reconnecting,
                retry_at,
            );
            let sleep = async {
                match wait {
                    Some(d) => tokio::time::sleep(d).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = self.shutdown.cancelled() => return,
                () = sleep => {}
                () = self.network.notified() => {
                    tracing::debug!("network changed: reconnecting now");
                }
                r = wait_identity_change(&mut sessions, &session) => {
                    if r.is_err() { return }
                    attempt = 0;
                }
            }
        }
    }

    async fn connect_and_run(
        &mut self,
        session: &ServerSession,
        sessions: &mut watch::Receiver<Option<ServerSession>>,
    ) -> Ended {
        let connect = tokio::time::timeout(self.timing.connect_timeout, self.connect(session));
        let (mut ws, hello) = tokio::select! {
            () = self.shutdown.cancelled() => return Ended::Shutdown,
            r = wait_identity_change(sessions, session) => {
                return if r.is_err() { Ended::Shutdown } else { Ended::SessionChanged };
            }
            r = connect => match r {
                Ok(Ok(c)) => c,
                Ok(Err(Connect::Unauthorized)) => return Ended::Revoked,
                Ok(Err(Connect::Failed(reason))) => {
                    tracing::debug!(%reason, server_id = %session.server_id, "realtime connect failed");
                    return Ended::Failed;
                }
                Err(_) => {
                    tracing::debug!(server_id = %session.server_id, "realtime connect timed out");
                    return Ended::Failed;
                }
            },
        };
        tracing::info!(server_id = %session.server_id, "realtime connected");
        // Drop frames queued for a previous connection.
        while self.outgoing.try_recv().is_ok() {}
        self.set_state(
            Some(session.server_id),
            SocialConnectionState::Connected,
            None,
        );
        if self
            .incoming
            .send(Incoming::Connected {
                server_id: session.server_id,
                hello,
            })
            .await
            .is_err()
        {
            return Ended::Shutdown;
        }
        loop {
            tokio::select! {
                () = self.shutdown.cancelled() => {
                    let _ = ws.close(None).await;
                    return Ended::Shutdown;
                }
                r = wait_identity_change(sessions, session) => {
                    let _ = ws.close(None).await;
                    return if r.is_err() { Ended::Shutdown } else { Ended::SessionChanged };
                }
                Some((kind, data)) = self.outgoing.recv() => {
                    let frame = Envelope { v: 1, id: None, kind, ts: None, data };
                    let Ok(text) = serde_json::to_string(&frame) else { continue };
                    if ws.send(Message::Text(text.into())).await.is_err() {
                        return Ended::AfterHello;
                    }
                }
                frame = tokio::time::timeout(self.timing.dead_after, ws.next()) => match frame {
                    Err(_) => {
                        tracing::info!(server_id = %session.server_id, "realtime socket silent; reconnecting");
                        return Ended::AfterHello;
                    }
                    Ok(None) | Ok(Some(Err(_))) => return Ended::AfterHello,
                    Ok(Some(Ok(Message::Close(frame)))) => {
                        let code = frame.as_ref().map(|f| u16::from(f.code));
                        tracing::info!(server_id = %session.server_id, ?code, "realtime socket closed by the server");
                        return if code == Some(close::SESSION_REVOKED) { Ended::Revoked } else { Ended::AfterHello };
                    }
                    Ok(Some(Ok(Message::Text(text)))) => {
                        let Ok(env) = serde_json::from_str::<Envelope>(text.as_str()) else {
                            tracing::debug!("ignoring an unreadable realtime frame");
                            continue;
                        };
                        if env.kind == "pong" || env.kind == "error" {
                            continue;
                        }
                        if env.kind == kinds::SESSION_REVOKED {
                            // The 4001 close follows, but a reset can lose it (seen on Windows).
                            tracing::info!(server_id = %session.server_id, "realtime session revoked");
                            let _ = ws.close(None).await;
                            return Ended::Revoked;
                        }
                        let event = Incoming::Event { server_id: session.server_id, kind: env.kind, data: env.data };
                        if self.incoming.send(event).await.is_err() {
                            return Ended::Shutdown;
                        }
                    }
                    Ok(Some(Ok(_))) => {} // ping/pong/binary: any frame proves the socket alive
                },
            }
        }
    }

    async fn connect(&self, session: &ServerSession) -> Result<(WsStream, Hello), Connect> {
        let api = SocialApi {
            http: &self.http,
            session: session.clone(),
            sessions: &self.sessions,
        };
        let ticket = match api.realtime_ticket().await {
            Ok(t) => t,
            Err(SocialError::NotSignedIn) => return Err(Connect::Unauthorized),
            Err(e) => return Err(Connect::Failed(e.to_string())),
        };
        let mut url = session
            .base_url
            .join("v1/realtime")
            .map_err(|e| Connect::Failed(e.to_string()))?;
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme)
            .map_err(|()| Connect::Failed("bad scheme".into()))?;
        url.query_pairs_mut().append_pair("ticket", &ticket.ticket);
        let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .map_err(|e| match e {
                tokio_tungstenite::tungstenite::Error::Http(r) if r.status() == 401 => {
                    Connect::Unauthorized
                }
                other => Connect::Failed(other.to_string()),
            })?;
        // The first frame must be `hello`.
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    let env: Envelope = serde_json::from_str(text.as_str())
                        .map_err(|e| Connect::Failed(e.to_string()))?;
                    if env.kind != "hello" {
                        return Err(Connect::Failed(format!("expected hello, got {}", env.kind)));
                    }
                    let hello: Hello = serde_json::from_value(env.data)
                        .map_err(|e| Connect::Failed(e.to_string()))?;
                    return Ok((ws, hello));
                }
                Some(Ok(Message::Close(frame))) => {
                    return Err(match frame.map(|f| f.code) {
                        Some(CloseCode::Library(close::SESSION_REVOKED)) => Connect::Unauthorized,
                        other => Connect::Failed(format!("closed before hello: {other:?}")),
                    });
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(Connect::Failed(e.to_string())),
                None => return Err(Connect::Failed("closed before hello".into())),
            }
        }
    }
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

enum Connect {
    Unauthorized,
    Failed(String),
}

/// Resolves when the session goes away or changes server/user (not on a token refresh).
async fn wait_identity_change(
    sessions: &mut watch::Receiver<Option<ServerSession>>,
    current: &ServerSession,
) -> Result<(), watch::error::RecvError> {
    loop {
        sessions.changed().await?;
        let changed = match &*sessions.borrow() {
            Some(s) => !s.same_identity(current),
            None => true,
        };
        if changed {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_stays_within_bounds_and_grows() {
        let t = Timing::default();
        for attempt in 0..40 {
            let cap = Duration::from_secs(1u64 << attempt.min(6)).min(Duration::from_secs(60));
            for _ in 0..50 {
                let d = backoff(&t, attempt);
                assert!(
                    d >= Duration::from_secs(1) && d <= cap,
                    "attempt {attempt}: {d:?}"
                );
            }
        }
        assert_eq!(backoff(&t, 0), Duration::from_secs(1));
        let late: Vec<Duration> = (0..200).map(|_| backoff(&t, 10)).collect();
        assert!(
            late.iter().any(|d| *d > Duration::from_secs(30)),
            "jitter spans the range"
        );
    }
}
