//! Per-instance registry of open sockets and the inbound message router.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use futures_util::future::BoxFuture;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{error::ApiError, state::AppState};

/// Messages queued for one socket.
#[derive(Debug, Clone)]
pub enum Outbound {
    /// A serialized envelope.
    Event(Arc<str>),
    /// Close with a code and reason.
    Close(u16, &'static str),
}

/// Per-socket send queue capacity. A socket that falls this far behind is dropped;
/// the client reconnects and resyncs over REST.
pub const QUEUE_CAPACITY: usize = 256;

#[derive(Clone)]
struct Conn {
    id: u64,
    session_id: Uuid,
    tx: mpsc::Sender<Outbound>,
}

/// Who an event goes to.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Target {
    pub users: Vec<Uuid>,
    /// When set, only sockets of these sessions receive the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<Uuid>>,
    /// Close the matching sockets after delivering the event.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub close: bool,
}

impl Target {
    pub fn users(users: &[Uuid]) -> Self {
        Self {
            users: users.to_vec(),
            sessions: None,
            close: false,
        }
    }
}

/// Context passed to inbound handlers.
#[derive(Clone)]
pub struct InboundContext {
    pub state: AppState,
    pub user_id: Uuid,
    pub session_id: Uuid,
    pub device_id: Option<Uuid>,
}

/// Handles one client→server event type (e.g. Agent 4's `presence.set`).
pub type InboundHandler = Arc<
    dyn Fn(InboundContext, serde_json::Value) -> BoxFuture<'static, Result<(), ApiError>>
        + Send
        + Sync,
>;

/// Wraps an async fn as an [`InboundHandler`].
pub fn handler<F, Fut>(f: F) -> InboundHandler
where
    F: Fn(InboundContext, serde_json::Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), ApiError>> + Send + 'static,
{
    Arc::new(move |ctx, data| Box::pin(f(ctx, data)))
}

pub struct Hub {
    conns: Mutex<HashMap<Uuid, Vec<Conn>>>,
    next_id: AtomicU64,
    handlers: HashMap<&'static str, InboundHandler>,
}

impl Hub {
    pub fn new(handlers: Vec<(&'static str, InboundHandler)>) -> Self {
        Self {
            conns: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            handlers: handlers.into_iter().collect(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Vec<Conn>>> {
        self.conns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Registers a socket; returns its id and receiving end.
    pub fn register(&self, user_id: Uuid, session_id: Uuid) -> (u64, mpsc::Receiver<Outbound>) {
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.lock()
            .entry(user_id)
            .or_default()
            .push(Conn { id, session_id, tx });
        (id, rx)
    }

    pub fn unregister(&self, user_id: Uuid, conn_id: u64) {
        let mut conns = self.lock();
        if let Some(list) = conns.get_mut(&user_id) {
            list.retain(|c| c.id != conn_id);
            if list.is_empty() {
                conns.remove(&user_id);
            }
        }
    }

    /// Number of open sockets on this instance.
    pub fn connection_count(&self) -> usize {
        self.lock().values().map(Vec::len).sum()
    }

    /// `true` when `user_id` has at least one socket on this instance.
    pub fn is_connected(&self, user_id: Uuid) -> bool {
        self.lock().get(&user_id).is_some_and(|l| !l.is_empty())
    }

    /// Users with at least one socket on this instance (presence heartbeats).
    pub fn connected_users(&self) -> Vec<Uuid> {
        self.lock()
            .iter()
            .filter(|(_, l)| !l.is_empty())
            .map(|(u, _)| *u)
            .collect()
    }

    /// Queues `event` on every matching local socket. Sockets whose queue is full are
    /// dropped (their task closes them with `TOO_SLOW`).
    pub fn dispatch(&self, target: &Target, event: Arc<str>) {
        let mut conns = self.lock();
        for user in &target.users {
            let Some(list) = conns.get_mut(user) else {
                continue;
            };
            list.retain(|c| {
                if target
                    .sessions
                    .as_ref()
                    .is_some_and(|s| !s.contains(&c.session_id))
                {
                    return true;
                }
                if c.tx.try_send(Outbound::Event(event.clone())).is_err() {
                    tracing::info!(%user, "dropping slow realtime consumer");
                    return false;
                }
                if target.close {
                    let _ = c.tx.try_send(Outbound::Close(
                        vgames_proto::realtime::close::SESSION_REVOKED,
                        "session revoked",
                    ));
                }
                true
            });
            if list.is_empty() {
                conns.remove(user);
            }
        }
    }

    /// Asks every local socket to close (shutdown).
    pub fn close_all(&self, code: u16, reason: &'static str) {
        for list in self.lock().values() {
            for c in list {
                let _ = c.tx.try_send(Outbound::Close(code, reason));
            }
        }
    }

    pub fn inbound_handler(&self, kind: &str) -> Option<InboundHandler> {
        self.handlers.get(kind).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_consumers_are_dropped_and_others_keep_receiving() {
        let hub = Hub::new(Vec::new());
        let user = Uuid::now_v7();
        let (slow_id, _slow_rx) = hub.register(user, Uuid::now_v7());
        let (_fast_id, mut fast_rx) = hub.register(user, Uuid::now_v7());
        let ev: Arc<str> = Arc::from("{}");
        for _ in 0..QUEUE_CAPACITY {
            hub.dispatch(&Target::users(&[user]), ev.clone());
            while fast_rx.try_recv().is_ok() {}
        }
        assert_eq!(hub.connection_count(), 2);
        hub.dispatch(&Target::users(&[user]), ev.clone());
        assert_eq!(hub.connection_count(), 1, "the slow socket is gone");
        assert!(fast_rx.try_recv().is_ok());
        hub.unregister(user, slow_id);
        assert_eq!(hub.connection_count(), 1);
    }

    #[test]
    fn session_targeting_and_close() {
        let hub = Hub::new(Vec::new());
        let user = Uuid::now_v7();
        let (s1, s2) = (Uuid::now_v7(), Uuid::now_v7());
        let (_a, mut rx1) = hub.register(user, s1);
        let (_b, mut rx2) = hub.register(user, s2);
        hub.dispatch(
            &Target {
                users: vec![user],
                sessions: Some(vec![s1]),
                close: true,
            },
            Arc::from("x"),
        );
        assert!(matches!(rx1.try_recv(), Ok(Outbound::Event(_))));
        assert!(matches!(rx1.try_recv(), Ok(Outbound::Close(4001, _))));
        assert!(rx2.try_recv().is_err());
    }
}
