//! Keeps the social [`SessionSlot`] in step with Agent 2's server sessions (A2-T07).
//!
//! The slot holds the active server, the signed-in user and a current access token. It is
//! recomputed at startup and whenever the bus says the servers changed (switch, sign-in,
//! sign-out, removal). When a social call or the realtime socket is refused (`401`, close
//! 4001), the token is refreshed once through the server's single-flight refresh; if the
//! server ended the session, the slot empties and social features report `not_signed_in`.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::ports::{ServerSession, SessionSlot};
use crate::api::ApiError;
use crate::events::{AppEvent, EventBus};
use crate::servers::Servers;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A signed-in session with the token generation it came from.
#[derive(Clone, Debug)]
pub struct Current {
    pub session: ServerSession,
    pub generation: u64,
}

/// Where sessions come from (Agent 2's `Servers` in the app, a fake in tests).
pub trait SessionSource: Send + Sync + 'static {
    /// The active server's signed-in session, if any.
    fn current(&self) -> BoxFuture<'_, Option<Current>>;
    /// A new token after `stale` was refused (`None`: signed out or unreachable).
    fn refresh<'a>(&'a self, stale: &'a Current) -> BoxFuture<'a, Option<Current>>;
}

/// [`SessionSource`] over the launcher's server registry.
pub struct ServersSource {
    pub servers: Arc<Servers>,
    pub http: reqwest::Client,
}

impl ServersSource {
    async fn load(&self, refresh_from: Option<u64>) -> Option<Current> {
        let id = match self.servers.active_id().await {
            Ok(Some(id)) => id,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(%error, "cannot read the active server");
                return None;
            }
        };
        let profile = self.servers.profile(id).await.ok().flatten()?;
        if profile.blocked_fingerprint.is_some() {
            return None;
        }
        let account = profile.account?;
        let base_url = Url::parse(&profile.url).ok()?;
        let api = self.servers.api(id).await.ok()?;
        let grant = match refresh_from {
            None => api.session().access_token(&self.http).await,
            Some(generation) => api.session().refresh(&self.http, generation).await,
        };
        let grant = match grant {
            Ok(g) => g,
            Err(ApiError::Unauthenticated) => return None,
            Err(error) => {
                tracing::info!(%error, server_id = %id, "no access token for social features");
                return None;
            }
        };
        Some(Current {
            session: ServerSession {
                server_id: id,
                user_id: account.user_id,
                base_url,
                access_token: Arc::new(Zeroizing::new(grant.token.to_string())),
            },
            generation: grant.generation,
        })
    }
}

impl SessionSource for ServersSource {
    fn current(&self) -> BoxFuture<'_, Option<Current>> {
        Box::pin(self.load(None))
    }

    fn refresh<'a>(&'a self, stale: &'a Current) -> BoxFuture<'a, Option<Current>> {
        Box::pin(async move {
            // The active server may have changed meanwhile: then load it as usual.
            let active = self.servers.active_id().await.ok().flatten();
            if active == Some(stale.session.server_id) {
                self.load(Some(stale.generation)).await
            } else {
                self.load(None).await
            }
        })
    }
}

/// Starts the bridge; it stops with `shutdown`.
pub fn spawn(
    source: Arc<dyn SessionSource>,
    slot: SessionSlot,
    bus: &EventBus,
    shutdown: CancellationToken,
) {
    let refused = Arc::new(Notify::new());
    let refused_server: Arc<Mutex<Option<Uuid>>> = Arc::default();
    {
        let refused = refused.clone();
        let refused_server = refused_server.clone();
        slot.on_unauthorized(move |server| {
            *refused_server
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(server);
            refused.notify_one();
        });
    }
    let events = bus.subscribe();
    tokio::spawn(run(source, slot, events, refused, refused_server, shutdown));
}

async fn run(
    source: Arc<dyn SessionSource>,
    slot: SessionSlot,
    mut events: broadcast::Receiver<AppEvent>,
    refused: Arc<Notify>,
    refused_server: Arc<Mutex<Option<Uuid>>>,
    shutdown: CancellationToken,
) {
    let mut current = source.current().await;
    slot.set(current.as_ref().map(|c| c.session.clone()));
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = refused.notified() => {
                let server = refused_server.lock().unwrap_or_else(PoisonError::into_inner).take();
                current = match &current {
                    Some(c) if Some(c.session.server_id) == server => source.refresh(c).await,
                    _ => source.current().await,
                };
            }
            event = events.recv() => match event {
                Ok(AppEvent::ServerSwitched(_) | AppEvent::ServersChanged(_) | AppEvent::AuthFinished(_)) => {
                    current = source.current().await;
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => current = source.current().await,
                Err(broadcast::error::RecvError::Closed) => return,
            },
        }
        let next = current.as_ref().map(|c| c.session.clone());
        let unchanged = match (slot.current(), &next) {
            (Some(a), Some(b)) => a.same_identity(b) && a.access_token == b.access_token,
            (None, None) => true,
            _ => false,
        };
        if !unchanged {
            slot.set(next);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::events::{ServerSwitched, ServersChanged};

    /// A fake registry: one server, signed in or not, whose tokens count up.
    struct Fake {
        signed_in: Mutex<bool>,
        generation: AtomicU64,
        refreshes: AtomicU64,
    }

    impl Fake {
        fn session(&self) -> Option<Current> {
            if !*self.signed_in.lock().unwrap() {
                return None;
            }
            let g = self.generation.load(Ordering::SeqCst);
            Some(Current {
                session: ServerSession {
                    server_id: Uuid::from_u128(1),
                    user_id: Uuid::from_u128(2),
                    base_url: "https://vgames.test/".parse().unwrap(),
                    access_token: Arc::new(Zeroizing::new(format!("token-{g}"))),
                },
                generation: g,
            })
        }
    }

    impl SessionSource for Fake {
        fn current(&self) -> BoxFuture<'_, Option<Current>> {
            Box::pin(async move { self.session() })
        }
        fn refresh<'a>(&'a self, stale: &'a Current) -> BoxFuture<'a, Option<Current>> {
            Box::pin(async move {
                self.refreshes.fetch_add(1, Ordering::SeqCst);
                let _ = self.generation.compare_exchange(
                    stale.generation,
                    stale.generation + 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
                self.session()
            })
        }
    }

    async fn token_becomes(slot: &SessionSlot, want: Option<&str>) {
        let mut rx = slot.subscribe();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let now = rx
                    .borrow_and_update()
                    .as_ref()
                    .map(|s| s.access_token.as_str().to_owned());
                if now.as_deref() == want {
                    return;
                }
                rx.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| panic!("slot never became {want:?}"));
    }

    #[tokio::test]
    async fn follows_sign_in_refreshes_refused_tokens_and_empties_on_sign_out() {
        let fake = Arc::new(Fake {
            signed_in: Mutex::new(false),
            generation: AtomicU64::new(7),
            refreshes: AtomicU64::new(0),
        });
        let slot = SessionSlot::new();
        let bus = EventBus::new();
        let stop = CancellationToken::new();
        spawn(fake.clone(), slot.clone(), &bus, stop.clone());
        token_becomes(&slot, None).await;

        // Sign-in finished: the bus says the servers changed.
        *fake.signed_in.lock().unwrap() = true;
        bus.publish(AppEvent::ServersChanged(ServersChanged {}));
        token_becomes(&slot, Some("token-7")).await;
        assert_eq!(slot.current().unwrap().user_id, Uuid::from_u128(2));

        // A refused token is refreshed once and replaces the old one.
        slot.report_unauthorized(Uuid::from_u128(1));
        token_becomes(&slot, Some("token-8")).await;
        assert_eq!(fake.refreshes.load(Ordering::SeqCst), 1);

        // A report for another server does not refresh this one.
        slot.report_unauthorized(Uuid::from_u128(99));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(fake.refreshes.load(Ordering::SeqCst), 1);
        token_becomes(&slot, Some("token-8")).await;

        // Signed out (or switched to a server without an account): no session.
        *fake.signed_in.lock().unwrap() = false;
        bus.publish(AppEvent::ServerSwitched(ServerSwitched { server_id: None }));
        token_becomes(&slot, None).await;
        stop.cancel();
    }
}
