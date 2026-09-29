//! The in-game side of the broker link (05-social §6.1), shared by every renderer backend.
//!
//! One background thread owns the socket: it connects to `VGAMES_OVERLAY_ENDPOINT`, sends
//! `Hello` with the token, keeps the latest [`View`], answers heartbeats, forwards queued
//! [`Action`]s and reconnects with backoff when the link drops. The render thread never
//! touches the socket and never blocks:
//!
//! - [`Link::should_draw`] is one relaxed atomic load: `false` means return from the hook at
//!   once (nothing allocated, nothing locked).
//! - [`Link::view`] hands out the current view (`Arc`, no copy) when something is visible.
//! - [`Link::act`] queues an action (bounded; extra actions are dropped).
//!
//! The link gives up for good after the broker refused the handshake a few times, or when a
//! frame breaks the protocol: the game keeps running, the overlay stays hidden.

use std::collections::VecDeque;
use std::io::{ErrorKind, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::protocol::{
    self, Action, DEAD_AFTER, FrameError, HEARTBEAT_EVERY, PROTOCOL_VERSION, RendererKind,
    TOKEN_LEN, ToBroker, ToRenderer, View,
};

/// Actions waiting for the link thread.
const MAX_QUEUED_ACTIONS: usize = 16;
/// The link thread wakes at least this often (toast expiry, heartbeats, queued actions).
const TICK: Duration = Duration::from_millis(250);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const MIN_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF: Duration = Duration::from_secs(10);
/// Handshakes refused in a row before the link gives up.
const MAX_REFUSALS: u32 = 3;

/// Where to connect, from the launch environment.
#[derive(Clone)]
pub struct Endpoint {
    pub addr: SocketAddr,
    pub token: [u8; TOKEN_LEN],
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("addr", &self.addr)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl Endpoint {
    /// `None` unless `VGAMES_OVERLAY=1` and a loopback endpoint and a valid token are set.
    pub fn from_env() -> Option<Self> {
        Self::from_vars(
            std::env::var(protocol::env::ENABLED).ok().as_deref(),
            std::env::var(protocol::env::ENDPOINT).ok().as_deref(),
            std::env::var(protocol::env::TOKEN).ok().as_deref(),
        )
    }

    pub fn from_vars(
        enabled: Option<&str>,
        endpoint: Option<&str>,
        token: Option<&str>,
    ) -> Option<Self> {
        if enabled? != "1" {
            return None;
        }
        let addr: SocketAddr = endpoint?.parse().ok()?;
        // Only ever the launcher on this machine.
        if !addr.ip().is_loopback() {
            return None;
        }
        Some(Self {
            addr,
            token: protocol::parse_token(token?)?,
        })
    }
}

struct State {
    view: Arc<View>,
    /// When each toast of the current view expires.
    toast_deadlines: Vec<Instant>,
    panel_open: bool,
    actions: VecDeque<Action>,
}

/// Shared between the link thread and the render thread.
pub struct Link {
    visible: AtomicBool,
    connected: AtomicBool,
    stopped: AtomicBool,
    state: Mutex<State>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Link {
    fn new() -> Self {
        Self {
            visible: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            state: Mutex::new(State {
                view: Arc::new(View::default()),
                toast_deadlines: Vec::new(),
                panel_open: false,
                actions: VecDeque::new(),
            }),
        }
    }

    /// Starts the link thread for `endpoint`.
    pub fn start(endpoint: Endpoint, renderer: RendererKind) -> Arc<Self> {
        let link = Arc::new(Self::new());
        let l = Arc::clone(&link);
        let spawned = thread::Builder::new()
            .name("vgames-overlay-link".into())
            .spawn(move || {
                // Never let a bug here take the game down.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(&l, &endpoint, renderer);
                }));
                l.stop();
            });
        if spawned.is_err() {
            link.stop();
        }
        link
    }

    /// The per-frame check: draw only when this is `true`.
    #[inline]
    pub fn should_draw(&self) -> bool {
        self.visible.load(Ordering::Relaxed)
    }

    /// Whether the panel (input capture) is open.
    pub fn panel_open(&self) -> bool {
        self.should_draw() && lock(&self.state).panel_open
    }

    /// The current view and which of its toasts are still up.
    pub fn view(&self) -> (Arc<View>, Vec<bool>) {
        let s = lock(&self.state);
        let now = Instant::now();
        let alive = s.toast_deadlines.iter().map(|d| *d > now).collect();
        (Arc::clone(&s.view), alive)
    }

    /// Queues a user action for the launcher (dropped when the queue is full or the link is
    /// down). Closing the panel also hides it at once.
    pub fn act(&self, action: Action) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        let mut s = lock(&self.state);
        if matches!(action, Action::ClosePanel) {
            s.panel_open = false;
            self.refresh(&s, Instant::now());
        }
        if s.actions.len() < MAX_QUEUED_ACTIONS {
            s.actions.push_back(action);
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// `true` once the link gave up (the overlay stays hidden for this game).
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.connected.store(false, Ordering::Relaxed);
        self.visible.store(false, Ordering::Relaxed);
    }

    fn refresh(&self, s: &State, now: Instant) {
        let visible = s.panel_open || s.toast_deadlines.iter().any(|d| *d > now);
        self.visible.store(visible, Ordering::Relaxed);
    }

    fn apply(&self, msg: ToRenderer) {
        let now = Instant::now();
        let mut s = lock(&self.state);
        match msg {
            ToRenderer::View(v) => {
                // Toasts already shown keep their deadline; new ones start now.
                let old: Vec<(uuid::Uuid, Instant)> = s
                    .view
                    .toasts
                    .iter()
                    .zip(&s.toast_deadlines)
                    .map(|(t, d)| (t.id, *d))
                    .collect();
                s.toast_deadlines = v
                    .toasts
                    .iter()
                    .map(|t| {
                        old.iter().find(|(id, _)| *id == t.id).map_or(
                            now + Duration::from_secs(u64::from(t.ttl_secs)),
                            |(_, d)| *d,
                        )
                    })
                    .collect();
                s.view = Arc::new(v);
            }
            ToRenderer::Panel { open } => s.panel_open = open,
            ToRenderer::Heartbeat | ToRenderer::Welcome { .. } => {}
        }
        self.refresh(&s, now);
    }

    fn take_actions(&self) -> Vec<Action> {
        lock(&self.state).actions.drain(..).collect()
    }

    fn tick(&self) {
        let s = lock(&self.state);
        self.refresh(&s, Instant::now());
    }

    fn disconnected(&self) {
        self.connected.store(false, Ordering::Relaxed);
        let mut s = lock(&self.state);
        s.panel_open = false;
        s.actions.clear();
        self.refresh(&s, Instant::now());
    }
}

enum Session {
    /// The link dropped after a good handshake: reconnect.
    Dropped,
    /// Refused or never welcomed.
    Refused,
    /// The broker broke the protocol: give up.
    Fatal,
}

fn run(link: &Link, endpoint: &Endpoint, renderer: RendererKind) {
    let mut backoff = MIN_BACKOFF;
    let mut refusals = 0u32;
    loop {
        match session(link, endpoint, renderer) {
            Session::Dropped => {
                refusals = 0;
                backoff = MIN_BACKOFF;
            }
            Session::Refused => {
                refusals += 1;
                if refusals >= MAX_REFUSALS {
                    return;
                }
            }
            Session::Fatal => return,
        }
        link.disconnected();
        thread::sleep(backoff);
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

fn session(link: &Link, endpoint: &Endpoint, renderer: RendererKind) -> Session {
    let Ok(mut stream) = TcpStream::connect_timeout(&endpoint.addr, CONNECT_TIMEOUT) else {
        return Session::Refused;
    };
    let _ = stream.set_nodelay(true);
    let hello = ToBroker::Hello {
        version: PROTOCOL_VERSION,
        token: endpoint.token,
        renderer,
    };
    if stream.set_read_timeout(Some(CONNECT_TIMEOUT)).is_err()
        || protocol::write_frame(&mut stream, &hello).is_err()
    {
        return Session::Refused;
    }
    match protocol::read_frame::<ToRenderer>(&mut stream) {
        Ok(ToRenderer::Welcome { version }) if version == PROTOCOL_VERSION => {}
        Ok(_) => return Session::Fatal,
        Err(_) => return Session::Refused,
    }
    link.connected.store(true, Ordering::Relaxed);
    if stream.set_read_timeout(Some(TICK)).is_err() {
        return Session::Dropped;
    }
    let mut reader = FrameReader::default();
    let mut last_heard = Instant::now();
    let mut last_sent = Instant::now();
    loop {
        match reader.poll(&mut stream) {
            Ok(Some(msg)) => {
                last_heard = Instant::now();
                if msg.validate().is_err() {
                    return Session::Fatal;
                }
                link.apply(msg);
            }
            Ok(None) => {}
            Err(FrameError::Io(e)) if e.kind() == ErrorKind::UnexpectedEof => {
                return Session::Dropped;
            }
            Err(FrameError::Io(_)) => return Session::Dropped,
            Err(_) => return Session::Fatal,
        }
        link.tick();
        let now = Instant::now();
        if now.duration_since(last_heard) > DEAD_AFTER {
            return Session::Dropped;
        }
        let mut out = link
            .take_actions()
            .into_iter()
            .map(ToBroker::Action)
            .collect::<Vec<_>>();
        if now.duration_since(last_sent) >= HEARTBEAT_EVERY {
            out.push(ToBroker::Heartbeat);
        }
        for msg in out {
            if msg.validate().is_err() {
                continue;
            }
            match protocol::encode(&msg) {
                Ok(bytes) => {
                    if stream.write_all(&bytes).is_err() {
                        return Session::Dropped;
                    }
                    last_sent = now;
                }
                Err(_) => continue,
            }
        }
    }
}

/// Reads frames across read timeouts without losing partial data.
#[derive(Default)]
struct FrameReader {
    buf: Vec<u8>,
    need: Option<usize>,
}

impl FrameReader {
    fn poll(&mut self, stream: &mut TcpStream) -> Result<Option<ToRenderer>, FrameError> {
        use std::io::Read;
        loop {
            let want = match self.need {
                None => 4,
                Some(n) => 4 + n,
            };
            if self.buf.len() >= want {
                if let Some(n) = self.need {
                    let payload = self.buf.get(4..4 + n).ok_or(FrameError::Malformed)?;
                    let msg = protocol::decode(payload)?;
                    self.buf.drain(..4 + n);
                    self.need = None;
                    return Ok(Some(msg));
                }
                let header: [u8; 4] = self
                    .buf
                    .get(..4)
                    .and_then(|h| h.try_into().ok())
                    .ok_or(FrameError::Malformed)?;
                self.need = Some(protocol::frame_len(header)?);
                continue;
            }
            let mut chunk = [0u8; 4096];
            let max = (want - self.buf.len()).min(chunk.len());
            let slot = chunk.get_mut(..max).ok_or(FrameError::Malformed)?;
            match stream.read(slot) {
                Ok(0) => return Err(FrameError::Io(ErrorKind::UnexpectedEof.into())),
                Ok(n) => self
                    .buf
                    .extend_from_slice(slot.get(..n).ok_or(FrameError::Malformed)?),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Ok(None);
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(FrameError::Io(e)),
            }
        }
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

    use std::net::TcpListener;

    use uuid::Uuid;

    use super::*;
    use crate::protocol::{Toast, ToastKind};

    const TOKEN: [u8; TOKEN_LEN] = [9; TOKEN_LEN];

    /// A minimal broker: welcome, then run `script` on the connection.
    fn broker(script: impl FnOnce(&mut TcpStream) + Send + 'static) -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            match protocol::read_frame::<ToBroker>(&mut s).unwrap() {
                ToBroker::Hello { token, version, .. } => {
                    assert_eq!(token, TOKEN);
                    assert_eq!(version, PROTOCOL_VERSION);
                }
                other => panic!("{other:?}"),
            }
            protocol::write_frame(
                &mut s,
                &ToRenderer::Welcome {
                    version: PROTOCOL_VERSION,
                },
            )
            .unwrap();
            script(&mut s);
        });
        addr
    }

    fn wait(what: &str, f: impl Fn() -> bool) {
        wait_for(what, Duration::from_secs(5), f);
    }

    fn wait_for(what: &str, limit: Duration, f: impl Fn() -> bool) {
        let until = Instant::now() + limit;
        while !f() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn toast(ttl: u16) -> Toast {
        Toast {
            id: Uuid::now_v7(),
            kind: ToastKind::Message,
            title: "Sam".into(),
            body: "gg".into(),
            ttl_secs: ttl,
        }
    }

    #[test]
    fn the_environment_must_name_a_loopback_broker_and_a_token() {
        let hex = protocol::token_hex(&TOKEN);
        let ok = Endpoint::from_vars(Some("1"), Some("127.0.0.1:4000"), Some(&hex)).unwrap();
        assert_eq!(ok.addr.port(), 4000);
        assert!(!format!("{ok:?}").contains(&hex));
        for (e, a, t) in [
            (None, Some("127.0.0.1:4000"), Some(hex.as_str())),
            (Some("0"), Some("127.0.0.1:4000"), Some(hex.as_str())),
            (Some("1"), Some("10.0.0.2:4000"), Some(hex.as_str())),
            (Some("1"), Some("localhost"), Some(hex.as_str())),
            (Some("1"), Some("127.0.0.1:4000"), Some("abc")),
            (Some("1"), None, Some(hex.as_str())),
        ] {
            assert!(Endpoint::from_vars(e, a, t).is_none(), "{e:?} {a:?} {t:?}");
        }
    }

    #[test]
    fn views_panel_toasts_and_actions() {
        let (tx, rx) = std::sync::mpsc::channel();
        let addr = broker(move |s| {
            let mut v = View::default();
            v.toasts.push(toast(1));
            protocol::write_frame(s, &ToRenderer::View(v)).unwrap();
            protocol::write_frame(s, &ToRenderer::Panel { open: true }).unwrap();
            loop {
                match protocol::read_frame::<ToBroker>(s) {
                    Ok(ToBroker::Action(a)) => tx.send(a).unwrap(),
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
        let link = Link::start(Endpoint { addr, token: TOKEN }, RendererKind::OpenGl);
        wait("the panel", || link.panel_open());
        assert!(link.should_draw());
        let (view, alive) = link.view();
        assert_eq!(view.toasts.len(), 1);
        assert_eq!(alive, vec![true]);
        link.act(Action::OpenLauncher);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Action::OpenLauncher
        );
        // Closing hides the panel at once; the toast still shows until it expires.
        link.act(Action::ClosePanel);
        assert!(!link.panel_open());
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Action::ClosePanel
        );
        wait("the toast to expire", || !link.should_draw());
        assert_eq!(link.view().1, vec![false]);
    }

    #[test]
    fn a_dropped_link_hides_the_overlay_and_reconnects() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let serve = move |l: &TcpListener, open: bool| {
            let (mut s, _) = l.accept().unwrap();
            protocol::read_frame::<ToBroker>(&mut s).unwrap();
            protocol::write_frame(
                &mut s,
                &ToRenderer::Welcome {
                    version: PROTOCOL_VERSION,
                },
            )
            .unwrap();
            protocol::write_frame(&mut s, &ToRenderer::Panel { open }).unwrap();
            s
        };
        let link = Link::start(Endpoint { addr, token: TOKEN }, RendererKind::Vulkan);
        let first = serve(&l, true);
        wait("the panel", || link.panel_open());
        drop(first);
        wait("the overlay to hide", || !link.should_draw());
        // The broker is back (swapchain recreated): the link connects again by itself.
        let _second = serve(&l, true);
        wait("the reconnect", || link.panel_open());
        assert!(!link.is_stopped());
    }

    #[test]
    fn refusals_and_protocol_errors_stop_the_link_for_good() {
        // Nobody listens: refused three times, then stopped.
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        let link = Link::start(
            Endpoint {
                addr: dead,
                token: TOKEN,
            },
            RendererKind::D3d11,
        );
        // Windows retries a refused connect for about 2 s, so three attempts plus backoff
        // take up to ~8 s there.
        wait_for("the link to give up", Duration::from_secs(20), || {
            link.is_stopped()
        });
        assert!(!link.should_draw());
        link.act(Action::OpenLauncher);

        // A broker that sends an oversized frame: fatal.
        let addr = broker(|s| {
            s.write_all(&(1u32 << 30).to_le_bytes()).unwrap();
            thread::sleep(Duration::from_secs(1));
        });
        let link = Link::start(Endpoint { addr, token: TOKEN }, RendererKind::D3d11);
        wait("the link to give up", || link.is_stopped());
    }
}
