//! The per-game overlay broker (05-social §6.1, A4-T10).
//!
//! - Listens on `127.0.0.1:<random>` with a fresh 32-byte token while one game runs; the game
//!   learns both from `VGAMES_OVERLAY_ENDPOINT` / `VGAMES_OVERLAY_TOKEN`.
//! - A connection must send `Hello` (right version, right token, compared in constant time)
//!   within [`HELLO_TIMEOUT`]; anything else closes it. After [`MAX_AUTH_FAILURES`] failed
//!   handshakes the broker stops for good (the game plays on without the overlay).
//! - Exactly one renderer is served at a time: after a good handshake the listener is closed.
//!   When that renderer goes away (the game recreated its swapchain or device), the broker
//!   binds the same port again and waits for it to come back with the same token.
//! - The renderer receives the whole view on connect and on every change, heartbeats every
//!   5 s, and `Panel` commands; its frames are untrusted (length before allocation, closed
//!   enums, caps) and a link silent for 15 s is dropped.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use vgames_overlay::protocol::{
    self, Action, DEAD_AFTER, FrameError, HEARTBEAT_EVERY, PROTOCOL_VERSION, RendererKind,
    TOKEN_LEN, ToBroker, ToRenderer,
};

use super::hub::to_protocol;
use super::model::OverlayView;

/// Time a new connection has to present its `Hello`.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Failed handshakes before the broker gives up.
pub const MAX_AUTH_FAILURES: u32 = 5;
/// Attempts to listen again on the same port after a renderer left.
const REBIND_TRIES: u32 = 10;
const REBIND_PAUSE: Duration = Duration::from_millis(100);

/// What a game launched with the overlay gets in its environment.
#[derive(Clone)]
pub struct Endpoint {
    pub addr: SocketAddr,
    token: [u8; TOKEN_LEN],
}

impl Endpoint {
    /// `VGAMES_OVERLAY=1`, `VGAMES_OVERLAY_ENDPOINT`, `VGAMES_OVERLAY_TOKEN`.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            (protocol::env::ENABLED.to_owned(), "1".to_owned()),
            (protocol::env::ENDPOINT.to_owned(), self.addr.to_string()),
            (
                protocol::env::TOKEN.to_owned(),
                protocol::token_hex(&self.token),
            ),
        ]
    }
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("addr", &self.addr)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Something that happened on the broker (for the launcher and tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrokerEvent {
    Connected(RendererKind),
    Disconnected,
    AuthFailed,
    /// Too many failed handshakes, or the port could not be bound again.
    Stopped,
}

/// A running broker; dropping it (or cancelling) stops it.
pub struct Broker {
    pub endpoint: Endpoint,
    stop: CancellationToken,
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl Broker {
    /// Binds a loopback port and starts serving `views` / `panel` to one renderer at a time.
    /// Renderer actions go to `actions`; state changes to `events`.
    pub async fn start(
        views: watch::Receiver<OverlayView>,
        panel: watch::Receiver<bool>,
        actions: mpsc::Sender<Action>,
        events: mpsc::UnboundedSender<BrokerEvent>,
        parent: &CancellationToken,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = listener.local_addr()?;
        let mut token = [0u8; TOKEN_LEN];
        getrandom::fill(&mut token).map_err(|e| std::io::Error::other(e.to_string()))?;
        let stop = parent.child_token();
        let endpoint = Endpoint { addr, token };
        let task = Task {
            token,
            addr,
            views,
            panel,
            actions,
            events,
            stop: stop.clone(),
        };
        tokio::spawn(task.run(listener));
        Ok(Self { endpoint, stop })
    }
}

struct Task {
    token: [u8; TOKEN_LEN],
    addr: SocketAddr,
    views: watch::Receiver<OverlayView>,
    panel: watch::Receiver<bool>,
    actions: mpsc::Sender<Action>,
    events: mpsc::UnboundedSender<BrokerEvent>,
    stop: CancellationToken,
}

async fn read_frame(stream: &mut TcpStream) -> Result<ToBroker, FrameError> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;
    let len = protocol::frame_len(header)?;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await?;
    let msg: ToBroker = protocol::decode(&payload)?;
    msg.validate()?;
    Ok(msg)
}

async fn write_frame(stream: &mut TcpStream, msg: &ToRenderer) -> Result<(), FrameError> {
    let bytes = protocol::encode(msg)?;
    stream.write_all(&bytes).await?;
    Ok(())
}

impl Task {
    async fn run(mut self, first: TcpListener) {
        let mut listener = Some(first);
        let mut failures = 0u32;
        loop {
            let Some(l) = listener.take() else {
                // Serve again on the same port after a renderer left (a few tries: the old
                // connection may still be closing).
                match self.rebind().await {
                    Some(l) => {
                        listener = Some(l);
                        continue;
                    }
                    None => {
                        let _ = self.events.send(BrokerEvent::Stopped);
                        return;
                    }
                }
            };
            let accepted = tokio::select! {
                () = self.stop.cancelled() => return,
                a = l.accept() => a,
            };
            let Ok((mut stream, peer)) = accepted else {
                listener = Some(l);
                continue;
            };
            if !peer.ip().is_loopback() {
                listener = Some(l);
                continue;
            }
            match tokio::time::timeout(HELLO_TIMEOUT, self.handshake(&mut stream)).await {
                Ok(Ok(kind)) => {
                    // One renderer at a time: stop listening while it is served.
                    drop(l);
                    let _ = self.events.send(BrokerEvent::Connected(kind));
                    let stopped = self.serve(stream).await;
                    let _ = self.events.send(BrokerEvent::Disconnected);
                    if stopped {
                        return;
                    }
                }
                Ok(Err(_)) | Err(_) => {
                    failures += 1;
                    let _ = self.events.send(BrokerEvent::AuthFailed);
                    tracing::warn!(failures, "overlay handshake refused");
                    if failures >= MAX_AUTH_FAILURES {
                        let _ = self.events.send(BrokerEvent::Stopped);
                        return;
                    }
                    listener = Some(l);
                }
            }
        }
    }

    async fn rebind(&self) -> Option<TcpListener> {
        let mut last = None;
        for _ in 0..REBIND_TRIES {
            match TcpListener::bind(self.addr).await {
                Ok(l) => return Some(l),
                Err(error) => last = Some(error),
            }
            tokio::select! {
                () = self.stop.cancelled() => return None,
                () = tokio::time::sleep(REBIND_PAUSE) => {}
            }
        }
        if let Some(error) = last {
            tracing::warn!(%error, "overlay broker cannot listen again; overlay off for this game");
        }
        None
    }

    async fn handshake(&self, stream: &mut TcpStream) -> Result<RendererKind, FrameError> {
        match read_frame(stream).await? {
            ToBroker::Hello {
                version,
                token,
                renderer,
            } if version == PROTOCOL_VERSION && protocol::token_eq(&token, &self.token) => {
                write_frame(
                    stream,
                    &ToRenderer::Welcome {
                        version: PROTOCOL_VERSION,
                    },
                )
                .await?;
                Ok(renderer)
            }
            _ => Err(FrameError::Malformed),
        }
    }

    /// Serves one renderer until it leaves. `true` when the broker itself is stopping.
    async fn serve(&mut self, stream: TcpStream) -> bool {
        let (mut rd, mut wr) = stream.into_split();
        let (in_tx, mut inbound) = mpsc::channel::<Result<ToBroker, FrameError>>(16);
        let reader = tokio::spawn(async move {
            loop {
                let mut header = [0u8; 4];
                let frame = async {
                    rd.read_exact(&mut header).await?;
                    let len = protocol::frame_len(header)?;
                    let mut payload = vec![0u8; len];
                    rd.read_exact(&mut payload).await?;
                    let msg: ToBroker = protocol::decode(&payload)?;
                    msg.validate()?;
                    Ok::<_, FrameError>(msg)
                }
                .await;
                let failed = frame.is_err();
                if in_tx.send(frame).await.is_err() || failed {
                    return;
                }
            }
        });
        let send = |msg: ToRenderer| async move { protocol::encode(&msg) };
        let first = to_protocol(&self.views.borrow_and_update(), OffsetDateTime::now_utc());
        let open = *self.panel.borrow_and_update();
        let mut ok = match send(ToRenderer::View(first)).await {
            Ok(b) => wr.write_all(&b).await.is_ok(),
            Err(_) => false,
        };
        if ok && open {
            ok = match send(ToRenderer::Panel { open }).await {
                Ok(b) => wr.write_all(&b).await.is_ok(),
                Err(_) => false,
            };
        }
        let mut beat = tokio::time::interval(HEARTBEAT_EVERY);
        beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut deadline = tokio::time::Instant::now() + DEAD_AFTER;
        let mut stopping = false;
        while ok {
            let out = tokio::select! {
                () = self.stop.cancelled() => { stopping = true; break; }
                () = tokio::time::sleep_until(deadline) => {
                    tracing::info!("overlay renderer silent; dropping the link");
                    break;
                }
                _ = beat.tick() => ToRenderer::Heartbeat,
                r = self.views.changed() => {
                    if r.is_err() { stopping = true; break; }
                    let view = to_protocol(&self.views.borrow_and_update(), OffsetDateTime::now_utc());
                    ToRenderer::View(view)
                }
                r = self.panel.changed() => {
                    if r.is_err() { stopping = true; break; }
                    ToRenderer::Panel { open: *self.panel.borrow_and_update() }
                }
                frame = inbound.recv() => {
                    match frame {
                        Some(Ok(ToBroker::Heartbeat)) => {
                            deadline = tokio::time::Instant::now() + DEAD_AFTER;
                        }
                        Some(Ok(ToBroker::Action(action))) => {
                            deadline = tokio::time::Instant::now() + DEAD_AFTER;
                            // Bounded: a flood of actions is dropped, never queued without end.
                            if self.actions.try_send(action).is_err() {
                                tracing::debug!("overlay action dropped (queue full)");
                            }
                        }
                        Some(Ok(ToBroker::Hello { .. })) | Some(Err(_)) | None => {
                            tracing::debug!("overlay renderer closed or sent a bad frame");
                            break;
                        }
                    }
                    continue;
                }
            };
            ok = match protocol::encode(&out) {
                Ok(bytes) => wr.write_all(&bytes).await.is_ok(),
                Err(error) => {
                    tracing::warn!(%error, "overlay frame not sent");
                    true
                }
            };
        }
        reader.abort();
        stopping
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::io::{Read, Write};

    use uuid::Uuid;

    use super::*;

    struct Rig {
        broker: Broker,
        views: watch::Sender<OverlayView>,
        panel: watch::Sender<bool>,
        actions: mpsc::Receiver<Action>,
        events: mpsc::UnboundedReceiver<BrokerEvent>,
    }

    async fn rig() -> Rig {
        let (views, vrx) = watch::channel(OverlayView::default());
        let (panel, prx) = watch::channel(false);
        let (atx, actions) = mpsc::channel(8);
        let (etx, events) = mpsc::unbounded_channel();
        let broker = Broker::start(vrx, prx, atx, etx, &CancellationToken::new())
            .await
            .unwrap();
        Rig {
            broker,
            views,
            panel,
            actions,
            events,
        }
    }

    impl Rig {
        fn token(&self) -> [u8; TOKEN_LEN] {
            let env = self.broker.endpoint.env();
            protocol::parse_token(&env[2].1).unwrap()
        }

        async fn event(&mut self) -> BrokerEvent {
            tokio::time::timeout(Duration::from_secs(10), self.events.recv())
                .await
                .expect("a broker event")
                .unwrap()
        }
    }

    /// Like a renderer coming back: retries while the broker is not listening yet.
    fn reconnect(addr: SocketAddr, token: [u8; TOKEN_LEN]) -> std::net::TcpStream {
        let until = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match connect(addr, token) {
                Ok(s) => return s,
                Err(e) if std::time::Instant::now() < until => {
                    let _ = e;
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("the broker never listened again: {e}"),
            }
        }
    }

    /// A fake renderer on a blocking socket (like the in-game side).
    fn connect(addr: SocketAddr, token: [u8; TOKEN_LEN]) -> std::io::Result<std::net::TcpStream> {
        let mut s = std::net::TcpStream::connect(addr)?;
        s.set_read_timeout(Some(Duration::from_secs(10)))?;
        protocol::write_frame(
            &mut s,
            &ToBroker::Hello {
                version: PROTOCOL_VERSION,
                token,
                renderer: RendererKind::Vulkan,
            },
        )
        .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(s)
    }

    fn read(s: &mut std::net::TcpStream) -> ToRenderer {
        protocol::read_frame(s).unwrap()
    }

    fn next_view(s: &mut std::net::TcpStream) -> protocol::View {
        loop {
            match read(s) {
                ToRenderer::View(v) => return v,
                ToRenderer::Heartbeat | ToRenderer::Panel { .. } => {}
                other => panic!("{other:?}"),
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn serves_views_panel_and_actions_to_an_authenticated_renderer() {
        let mut r = rig().await;
        let env = r.broker.endpoint.env();
        assert_eq!(env[0], ("VGAMES_OVERLAY".into(), "1".into()));
        assert!(r.broker.endpoint.addr.ip().is_loopback());
        assert_eq!(env[2].1.len(), 64);
        assert!(!format!("{:?}", r.broker.endpoint).contains(&env[2].1));

        let (addr, token) = (r.broker.endpoint.addr, r.token());
        let mut s = tokio::task::spawn_blocking(move || {
            let mut s = connect(addr, token).unwrap();
            assert_eq!(
                read(&mut s),
                ToRenderer::Welcome {
                    version: PROTOCOL_VERSION
                }
            );
            assert_eq!(next_view(&mut s), protocol::View::default());
            s
        })
        .await
        .unwrap();
        assert_eq!(
            r.event().await,
            BrokerEvent::Connected(RendererKind::Vulkan)
        );
        // Only one renderer: the listener is closed while it is served.
        assert!(std::net::TcpStream::connect(addr).is_err());

        let mut view = OverlayView::default();
        view.recent_messages
            .push(super::super::model::OverlayMessage {
                conversation_id: Uuid::from_u128(3),
                from: "sam".into(),
                text: "gg".into(),
                sent_at: "2026-09-29T10:00:00Z".into(),
            });
        r.views.send_replace(view);
        r.panel.send_replace(true);
        let (v, panel, s2) = tokio::task::spawn_blocking(move || {
            // The view and the panel change may arrive in either order.
            let (mut v, mut panel) = (None, None);
            while v.is_none() || panel.is_none() {
                match read(&mut s) {
                    ToRenderer::View(view) => v = Some(view),
                    ToRenderer::Panel { open } => panel = Some(open),
                    ToRenderer::Heartbeat => {}
                    other => panic!("{other:?}"),
                }
            }
            let (v, panel) = (v.unwrap(), panel.unwrap());
            protocol::write_frame(
                &mut s,
                &ToBroker::Action(Action::QuickReply {
                    conversation_id: Uuid::from_u128(3),
                    text: "on my way".into(),
                }),
            )
            .unwrap();
            (v, panel, s)
        })
        .await
        .unwrap();
        s = s2;
        assert_eq!(v.recent_messages[0].text, "gg");
        assert!(panel);
        let action = tokio::time::timeout(Duration::from_secs(5), r.actions.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(action, Action::QuickReply { text, .. } if text == "on my way"));
        drop(s);
        assert_eq!(r.event().await, BrokerEvent::Disconnected);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wrong_tokens_are_refused_and_the_broker_gives_up_after_five() {
        let mut r = rig().await;
        let addr = r.broker.endpoint.addr;
        let mut bad = r.token();
        bad[0] ^= 1;
        for _ in 0..(MAX_AUTH_FAILURES - 1) {
            tokio::task::spawn_blocking(move || {
                let mut s = connect(addr, bad).unwrap();
                let mut buf = [0u8; 1];
                // Closed without a Welcome.
                assert_eq!(s.read(&mut buf).unwrap_or(0), 0);
            })
            .await
            .unwrap();
            assert_eq!(r.event().await, BrokerEvent::AuthFailed);
        }
        // Silence is a failure too (no Hello within the timeout is covered by the same path);
        // garbage before the Hello as well.
        tokio::task::spawn_blocking(move || {
            let mut s = std::net::TcpStream::connect(addr).unwrap();
            s.write_all(&[0xff, 0xff, 0xff, 0x7f]).unwrap();
            let mut buf = [0u8; 1];
            assert_eq!(s.read(&mut buf).unwrap_or(0), 0);
        })
        .await
        .unwrap();
        assert_eq!(r.event().await, BrokerEvent::AuthFailed);
        assert_eq!(r.event().await, BrokerEvent::Stopped);
        // Even the right token is refused now: nobody listens any more.
        let token = r.token();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let res = tokio::task::spawn_blocking(move || {
            connect(addr, token).and_then(|mut s| {
                let mut buf = [0u8; 1];
                s.read(&mut buf)
            })
        })
        .await
        .unwrap();
        assert!(matches!(res, Err(_) | Ok(0)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oversized_frames_drop_the_link_and_the_renderer_can_reconnect() {
        let mut r = rig().await;
        let (addr, token) = (r.broker.endpoint.addr, r.token());
        tokio::task::spawn_blocking(move || {
            let mut s = connect(addr, token).unwrap();
            assert!(matches!(read(&mut s), ToRenderer::Welcome { .. }));
            next_view(&mut s);
            // A 1 GiB frame header: refused before anything is allocated.
            s.write_all(&(1u32 << 30).to_le_bytes()).unwrap();
            let mut buf = [0u8; 64];
            loop {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            r.event().await,
            BrokerEvent::Connected(RendererKind::Vulkan)
        );
        assert_eq!(r.event().await, BrokerEvent::Disconnected);

        // The game recreated its swapchain: the renderer comes back on the same endpoint.
        tokio::task::spawn_blocking(move || {
            let mut s = reconnect(addr, token);
            assert!(matches!(read(&mut s), ToRenderer::Welcome { .. }));
            next_view(&mut s);
            // An invalid action (empty reply) also drops the link.
            protocol::write_frame(
                &mut s,
                &ToBroker::Action(Action::QuickReply {
                    conversation_id: Uuid::nil(),
                    text: " ".into(),
                }),
            )
            .unwrap();
            let mut buf = [0u8; 64];
            loop {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            r.event().await,
            BrokerEvent::Connected(RendererKind::Vulkan)
        );
        assert_eq!(r.event().await, BrokerEvent::Disconnected);
        assert!(
            r.actions.try_recv().is_err(),
            "the invalid action was not forwarded"
        );

        // Stopping the broker (game exit) closes everything.
        let endpoint = r.broker.endpoint.clone();
        drop(r.broker);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let res = tokio::task::spawn_blocking(move || {
            connect(endpoint.addr, token).and_then(|mut s| {
                let mut buf = [0u8; 1];
                s.read(&mut buf)
            })
        })
        .await
        .unwrap();
        assert!(matches!(res, Err(_) | Ok(0)));
    }
}
