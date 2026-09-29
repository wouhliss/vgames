//! In-game overlay, launcher side (05-social §6, A4-T10). Owner: Agent 4.
//!
//! - `hub`: the view model (toasts, friends online, invites, recent messages) built from
//!   social events.
//! - `broker`: one loopback broker per game launched with the overlay; the in-game renderer
//!   (`crates/vgames-overlay`, A4-T11) connects to it.
//! - `valve`: per-package switches and the crash safety valve.
//! - `commands`: the overlay window's `overlay_view` / `overlay_action` and the Settings
//!   commands `package_overlays_list` / `package_overlay_set`.
//!
//! Launch integration: the service is the launcher's [`LaunchHooks`]. Before a game starts,
//! [`OverlayService::prepare_launch`] adds the overlay variables to its environment; a start
//! that fails drops the broker again, and `GameStopped` on the bus ends it and feeds the valve.

pub mod broker;
pub mod commands;
pub mod fallback;
pub mod hotkey;
pub mod hub;
pub mod model;
pub mod valve;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;
use vgames_overlay::protocol::Action;

use crate::db::{Db, now_unix, settings};
use crate::events::{AppEvent, ControllerChange, EventBus, PackageRef};
use crate::launch::orchestrate::LaunchHooks;
use crate::social::service::{SocialService, SocialSettingsKey};
use broker::{Broker, BrokerEvent};
use hub::Hub;
use model::{OverlayAction, PackageOverlay};

/// How long a launched game has to connect its in-game renderer before the fallback shows.
pub const FALLBACK_AFTER: std::time::Duration = std::time::Duration::from_secs(20);

impl LaunchHooks for OverlayService {
    fn prepare(&self, package: PackageRef) -> crate::launch::orchestrate::HookEnv<'_> {
        // The title is filled in by the Settings list from the library.
        Box::pin(self.prepare_launch(package, ""))
    }

    fn aborted(&self, package: PackageRef) {
        self.end_session(package);
    }
}

/// How the overlay shows when it is not drawn inside the game (05-social §6.2–§6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// macOS panel or an always-on-top window (Windows, X11).
    Window,
    /// OS notifications for toasts (Wayland: no window can stay above other apps).
    Notifications,
}

/// The fallback this desktop supports.
pub fn fallback_for_this_desktop() -> Fallback {
    fallback_for(
        cfg!(target_os = "linux"),
        std::env::var_os("XDG_SESSION_TYPE").as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        std::env::var_os("DISPLAY").is_some(),
    )
}

fn fallback_for(
    linux: bool,
    session_type: Option<&std::ffi::OsStr>,
    wayland_display: bool,
    x_display: bool,
) -> Fallback {
    if !linux {
        return Fallback::Window;
    }
    let wayland = session_type.is_some_and(|t| t == "wayland") || (wayland_display && !x_display);
    if wayland {
        Fallback::Notifications
    } else {
        Fallback::Window
    }
}

/// Actions waiting to run (renderer floods are dropped beyond this).
const ACTION_QUEUE: usize = 32;

type Callback = Box<dyn Fn() + Send + Sync>;
type PackageCallback = Box<dyn Fn(PackageRef) + Send + Sync>;
type FallbackCallback = Box<dyn Fn(Option<Fallback>) + Send + Sync>;

struct Session {
    /// `None` when the broker could not start (the game runs without the overlay).
    _broker: Option<Broker>,
}

struct Inner {
    db: Db,
    social: SocialService,
    hub: Arc<Hub>,
    panel: watch::Sender<bool>,
    actions: mpsc::Sender<Action>,
    sessions: Mutex<HashMap<PackageRef, Session>>,
    shutdown: CancellationToken,
    open_launcher: Mutex<Option<Callback>>,
    valve_tripped: Mutex<Option<PackageCallback>>,
    hotkeys: Mutex<hotkey::HotkeyState>,
    /// Shows (`Some`) or hides (`None`) the fallback.
    fallback: Mutex<Option<FallbackCallback>>,
    /// Packages whose game shows the fallback instead of an in-game renderer.
    fallback_for: Mutex<std::collections::HashSet<PackageRef>>,
    fallback_after: Mutex<std::time::Duration>,
}

/// Cheap to clone.
#[derive(Clone)]
pub struct OverlayService {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl OverlayService {
    /// Starts the action and bus loops. `hub` must receive the social events (see
    /// `social::service::FanOut`).
    pub fn start(
        db: Db,
        social: SocialService,
        hub: Arc<Hub>,
        bus: &EventBus,
        shutdown: CancellationToken,
    ) -> Self {
        let (actions, rx) = mpsc::channel(ACTION_QUEUE);
        let service = Self {
            inner: Arc::new(Inner {
                db,
                social,
                hub,
                panel: watch::channel(false).0,
                actions,
                sessions: Mutex::new(HashMap::new()),
                shutdown: shutdown.clone(),
                open_launcher: Mutex::new(None),
                valve_tripped: Mutex::new(None),
                hotkeys: Mutex::new(hotkey::HotkeyState::new()),
                fallback: Mutex::new(None),
                fallback_for: Mutex::new(std::collections::HashSet::new()),
                fallback_after: Mutex::new(FALLBACK_AFTER),
            }),
        };
        tokio::spawn(service.clone().action_loop(rx, shutdown.clone()));
        tokio::spawn(service.clone().bus_loop(bus.subscribe(), shutdown));
        service
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.inner.hub
    }

    /// Where hotkeys are registered, and the configured accelerator (from the settings).
    pub fn set_hotkey_host(&self, host: Arc<dyn hotkey::HotkeyHost>, accelerator: &str) {
        let mut hk = lock(&self.inner.hotkeys);
        hk.host = Some(host);
        if let Ok(s) = hotkey::validate(accelerator) {
            hk.current = s;
        }
    }

    /// Shows or hides the fallback (overlay window or notifications).
    pub fn on_fallback(&self, f: impl Fn(Option<Fallback>) + Send + Sync + 'static) {
        *lock(&self.inner.fallback) = Some(Box::new(f));
    }

    fn fallback_on(&self, package: PackageRef) {
        let first = {
            let mut set = lock(&self.inner.fallback_for);
            let first = set.is_empty();
            set.insert(package);
            first
        };
        if first && let Some(f) = lock(&self.inner.fallback).as_ref() {
            f(Some(fallback_for_this_desktop()));
        }
    }

    fn fallback_off(&self, package: PackageRef) {
        let last = {
            let mut set = lock(&self.inner.fallback_for);
            set.remove(&package) && set.is_empty()
        };
        if last && let Some(f) = lock(&self.inner.fallback).as_ref() {
            f(None);
        }
    }

    /// Checks and adopts a new hotkey (before the settings are saved).
    pub fn change_hotkey(&self, accelerator: &str) -> Result<(), hotkey::HotkeyError> {
        lock(&self.inner.hotkeys).change(accelerator)
    }

    /// What "Open vgames" does (show the main window).
    pub fn on_open_launcher(&self, f: impl Fn() + Send + Sync + 'static) {
        *lock(&self.inner.open_launcher) = Some(Box::new(f));
    }

    /// Called when the safety valve turns the overlay off for a package (notice + re-enable).
    pub fn on_valve_tripped(&self, f: impl Fn(PackageRef) + Send + Sync + 'static) {
        *lock(&self.inner.valve_tripped) = Some(Box::new(f));
    }

    /// Before launching `package`: the environment variables that enable the overlay, or
    /// nothing when it is off (globally, for this package, or the broker cannot start). The
    /// launch goes on either way.
    pub async fn prepare_launch(&self, package: PackageRef, title: &str) -> Vec<(String, String)> {
        let title = title.to_owned();
        let enabled = self
            .inner
            .db
            .call(move |c| {
                let global = settings::get::<SocialSettingsKey>(c)?.overlay_enabled;
                let mut rows = settings::get::<valve::OverlayPackages>(c)?;
                valve::set_title(&mut rows, package, &title);
                settings::set::<valve::OverlayPackages>(c, &rows)?;
                Ok(global && valve::is_enabled(&rows, package))
            })
            .await
            .unwrap_or(false);
        if !enabled {
            return Vec::new();
        }
        lock(&self.inner.hotkeys).activate();
        if cfg!(target_os = "macos") {
            // Nothing is injected on macOS: the panel draws above fullscreen Spaces.
            lock(&self.inner.sessions).insert(package, Session { _broker: None });
            self.fallback_on(package);
            return Vec::new();
        }
        let (etx, erx) = mpsc::unbounded_channel();
        let broker = match Broker::start(
            self.inner.hub.subscribe(),
            self.inner.panel.subscribe(),
            self.inner.actions.clone(),
            etx,
            &self.inner.shutdown,
        )
        .await
        {
            Ok(b) => b,
            Err(error) => {
                tracing::warn!(%error, "overlay broker not started; launching without the overlay");
                return Vec::new();
            }
        };
        let env = broker.endpoint.env();
        tokio::spawn(self.clone().watch_broker(package, erx));
        lock(&self.inner.sessions).insert(
            package,
            Session {
                _broker: Some(broker),
            },
        );
        env
    }

    /// Opens or closes the panel everywhere (overlay window and in-game renderer).
    pub fn set_panel(&self, open: bool) {
        self.inner.hub.set_panel(open);
        self.inner.panel.send_replace(open);
    }

    /// The hotkey or the Guide/PS hold: open or close the panel while a game runs.
    pub fn toggle_panel(&self) {
        if lock(&self.inner.sessions).is_empty() {
            return;
        }
        let open = !*self.inner.panel.borrow();
        self.set_panel(open);
    }

    /// Queues an action from the overlay window.
    pub fn act(&self, action: OverlayAction) {
        let a = match action {
            OverlayAction::AcceptInvite { invite_id } => Action::AcceptInvite { invite_id },
            OverlayAction::DeclineInvite { invite_id } => Action::DeclineInvite { invite_id },
            OverlayAction::QuickReply {
                conversation_id,
                text,
            } => Action::QuickReply {
                conversation_id,
                text,
            },
            OverlayAction::OpenLauncher => Action::OpenLauncher,
            OverlayAction::ClosePanel => Action::ClosePanel,
        };
        if self.inner.actions.try_send(a).is_err() {
            tracing::debug!("overlay action dropped (queue full)");
        }
    }

    async fn run(&self, action: Action) {
        let s = &self.inner.social;
        let result = match action {
            Action::AcceptInvite { invite_id } => s.invite_accept(invite_id).await.map(drop),
            Action::DeclineInvite { invite_id } => s.invite_decline(invite_id).await.map(drop),
            Action::QuickReply {
                conversation_id,
                text,
            } => s.message_send(conversation_id, text).await.map(drop),
            Action::OpenLauncher => {
                self.set_panel(false);
                if let Some(f) = lock(&self.inner.open_launcher).as_ref() {
                    f();
                }
                Ok(())
            }
            Action::ClosePanel => {
                self.set_panel(false);
                Ok(())
            }
        };
        if let Err(error) = result {
            tracing::info!(%error, "overlay action failed");
        }
    }

    async fn action_loop(self, mut rx: mpsc::Receiver<Action>, shutdown: CancellationToken) {
        loop {
            let action = tokio::select! {
                () = shutdown.cancelled() => return,
                a = rx.recv() => match a { Some(a) => a, None => return },
            };
            self.run(action).await;
        }
    }

    async fn bus_loop(self, mut bus: broadcast::Receiver<AppEvent>, shutdown: CancellationToken) {
        loop {
            let event = tokio::select! {
                () = shutdown.cancelled() => return,
                e = bus.recv() => match e {
                    Ok(e) => e,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                },
            };
            match event {
                AppEvent::GameStopped(stopped) => {
                    if self.end_session(stopped.package) {
                        self.record_exit(stopped.package, stopped.exit).await;
                    }
                }
                AppEvent::Controller(c) if matches!(c.change, ControllerChange::GuideHeld) => {
                    self.toggle_panel();
                }
                _ => {}
            }
        }
    }

    async fn record_exit(&self, package: PackageRef, exit: crate::events::GameExit) {
        let now = now_unix();
        let tripped = self
            .inner
            .db
            .call(move |c| {
                let mut rows = settings::get::<valve::OverlayPackages>(c)?;
                let tripped = valve::record_exit(&mut rows, package, &exit, now);
                settings::set::<valve::OverlayPackages>(c, &rows)?;
                Ok(tripped)
            })
            .await
            .unwrap_or(false);
        if tripped && let Some(f) = lock(&self.inner.valve_tripped).as_ref() {
            f(package);
        }
    }

    /// Settings → Overlay: every package with a stored switch, newest titles first.
    pub async fn packages(&self) -> Result<Vec<PackageOverlay>, crate::db::DbError> {
        let rows = self
            .inner
            .db
            .call(|c| settings::get::<valve::OverlayPackages>(c))
            .await?;
        let mut out: Vec<PackageOverlay> = rows
            .into_iter()
            .map(|r| PackageOverlay {
                package: r.package(),
                title: r.title.clone(),
                enabled: r.enabled,
                disabled_by_safety_valve_at: r
                    .disabled_by_valve_at
                    .map(crate::social::model::unix_rfc3339),
            })
            .collect();
        out.sort_by_key(|p| p.title.to_lowercase());
        Ok(out)
    }

    pub async fn package_set(
        &self,
        package: PackageRef,
        enabled: bool,
    ) -> Result<(), crate::db::DbError> {
        self.inner
            .db
            .call(move |c| {
                let mut rows = settings::get::<valve::OverlayPackages>(c)?;
                valve::set_enabled(&mut rows, package, enabled);
                settings::set::<valve::OverlayPackages>(c, &rows)
            })
            .await
    }
}

impl OverlayService {
    /// Logs the broker and switches to the fallback when no in-game renderer shows up within
    /// [`FALLBACK_AFTER`] (injection or layer failed or is off) or the broker gives up; a
    /// renderer that connects later takes over again.
    /// Drops the broker and fallback of `package`; `true` when it had an overlay session.
    fn end_session(&self, package: PackageRef) -> bool {
        let had = lock(&self.inner.sessions).remove(&package).is_some();
        if lock(&self.inner.sessions).is_empty() {
            self.set_panel(false);
            lock(&self.inner.hotkeys).deactivate();
        }
        self.fallback_off(package);
        had
    }

    async fn watch_broker(self, package: PackageRef, mut rx: mpsc::UnboundedReceiver<BrokerEvent>) {
        let wait = *lock(&self.inner.fallback_after);
        let deadline = tokio::time::sleep(wait);
        tokio::pin!(deadline);
        let mut connected_once = false;
        loop {
            let event = tokio::select! {
                () = &mut deadline, if !connected_once => {
                    if lock(&self.inner.sessions).contains_key(&package) {
                        tracing::info!("no in-game overlay renderer; using the fallback");
                        self.fallback_on(package);
                    }
                    connected_once = true;
                    continue;
                }
                e = rx.recv() => match e { Some(e) => e, None => return },
            };
            match event {
                BrokerEvent::Connected(kind) => {
                    tracing::info!(?kind, "overlay renderer connected");
                    connected_once = true;
                    self.fallback_off(package);
                }
                BrokerEvent::Disconnected => tracing::info!("overlay renderer disconnected"),
                BrokerEvent::AuthFailed => tracing::warn!("overlay connection refused"),
                BrokerEvent::Stopped => {
                    tracing::warn!("overlay broker stopped");
                    if lock(&self.inner.sessions).contains_key(&package) {
                        self.fallback_on(package);
                    }
                }
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

    use std::ffi::OsStr;
    use std::time::Duration;

    use uuid::Uuid;
    use vgames_overlay::protocol::{self, PROTOCOL_VERSION, RendererKind, ToBroker, ToRenderer};

    use super::*;
    use crate::events::{GameExit, GameStopped};
    use crate::social::crypto::SecretKey32;
    use crate::social::ports::{IdleSource, SessionSlot};
    use crate::social::realtime::Timing;
    use crate::social::store::Keys;

    struct NoIdle;
    impl IdleSource for NoIdle {
        fn idle_for(&self) -> Option<Duration> {
            None
        }
    }

    async fn service() -> (
        OverlayService,
        EventBus,
        mpsc::UnboundedReceiver<Option<Fallback>>,
        mpsc::UnboundedReceiver<PackageRef>,
    ) {
        let db = Db::open_in_memory().unwrap();
        let bus = EventBus::new();
        let hub = Arc::new(Hub::new());
        let stop = CancellationToken::new();
        let social = SocialService::start(
            SessionSlot::new(),
            db.clone(),
            &bus,
            hub.clone(),
            Arc::new(NoIdle),
            Arc::new(
                Keys::new(
                    SecretKey32::from_bytes([1; 32]),
                    &SecretKey32::from_bytes([2; 32]),
                )
                .unwrap(),
            ),
            "Test PC".into(),
            Timing::default(),
            stop.clone(),
        )
        .await
        .unwrap();
        let overlay = OverlayService::start(db, social, hub, &bus, stop);
        *lock(&overlay.inner.fallback_after) = Duration::from_millis(300);
        let (ftx, frx) = mpsc::unbounded_channel();
        overlay.on_fallback(move |m| {
            let _ = ftx.send(m);
        });
        let (vtx, vrx) = mpsc::unbounded_channel();
        overlay.on_valve_tripped(move |p| {
            let _ = vtx.send(p);
        });
        (overlay, bus, frx, vrx)
    }

    async fn next<T>(rx: &mut mpsc::UnboundedReceiver<T>) -> T {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("in time")
            .unwrap()
    }

    fn crash(package: PackageRef) -> AppEvent {
        AppEvent::GameStopped(GameStopped {
            package,
            exit: GameExit {
                code: Some(-1),
                stopped_by_user: false,
                session_seconds: 4,
            },
        })
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn launch_fallback_renderer_takeover_and_the_safety_valve() {
        let (overlay, bus, mut fallback, mut tripped) = service().await;
        let game = PackageRef {
            server_id: Uuid::from_u128(1),
            package_id: Uuid::from_u128(2),
        };
        let env = overlay.prepare_launch(game, "Arena").await;
        if cfg!(target_os = "macos") {
            assert!(env.is_empty());
            assert!(next(&mut fallback).await.is_some());
            return;
        }
        assert_eq!(env.len(), 3);
        // No renderer shows up: the fallback appears.
        assert_eq!(next(&mut fallback).await, Some(fallback_for_this_desktop()));

        // A renderer connects late: the fallback goes away.
        let addr: std::net::SocketAddr = env[1].1.parse().unwrap();
        let token = protocol::parse_token(&env[2].1).unwrap();
        let renderer = tokio::task::spawn_blocking(move || {
            let mut s = std::net::TcpStream::connect(addr).unwrap();
            protocol::write_frame(
                &mut s,
                &ToBroker::Hello {
                    version: PROTOCOL_VERSION,
                    token,
                    renderer: RendererKind::D3d11,
                },
            )
            .unwrap();
            assert!(matches!(
                protocol::read_frame::<ToRenderer>(&mut s).unwrap(),
                ToRenderer::Welcome { .. }
            ));
            s
        })
        .await
        .unwrap();
        assert_eq!(next(&mut fallback).await, None);
        // The panel toggles only while a game runs; the renderer is told.
        overlay.toggle_panel();
        assert!(overlay.hub().current().visible_panel);
        drop(renderer);

        // Two quick crashes in a row with the overlay on: turned off for this game.
        bus.publish(crash(game));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !overlay.hub().current().visible_panel,
            "the panel closes with the last game"
        );
        assert_eq!(overlay.prepare_launch(game, "Arena").await.len(), 3);
        bus.publish(crash(game));
        assert_eq!(next(&mut tripped).await, game);
        let list = overlay.packages().await.unwrap();
        assert_eq!(list[0].title, "Arena");
        assert!(!list[0].enabled);
        assert!(list[0].disabled_by_safety_valve_at.is_some());
        // Off: no overlay environment for the next launch.
        assert!(overlay.prepare_launch(game, "Arena").await.is_empty());
        // One click turns it back on.
        overlay.package_set(game, true).await.unwrap();
        assert!(
            overlay.packages().await.unwrap()[0]
                .disabled_by_safety_valve_at
                .is_none()
        );
        assert_eq!(overlay.prepare_launch(game, "Arena").await.len(), 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_launch_that_fails_drops_the_broker() {
        let (overlay, _bus, mut fallback, _tripped) = service().await;
        let game = PackageRef {
            server_id: Uuid::from_u128(1),
            package_id: Uuid::from_u128(3),
        };
        let env = LaunchHooks::prepare(&overlay, game).await;
        LaunchHooks::aborted(&overlay, game);
        if cfg!(target_os = "macos") {
            return;
        }
        // The fallback timer (300 ms here) finds no session: nothing shows.
        assert!(
            tokio::time::timeout(Duration::from_secs(1), fallback.recv())
                .await
                .is_err()
        );
        let addr: std::net::SocketAddr = env[1].1.parse().unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while tokio::net::TcpStream::connect(addr).await.is_ok() {
            assert!(
                std::time::Instant::now() < until,
                "the broker still listens"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[test]
    fn wayland_gets_notifications_everything_else_a_window() {
        assert_eq!(fallback_for(false, None, false, false), Fallback::Window);
        assert_eq!(
            fallback_for(true, Some(OsStr::new("x11")), false, true),
            Fallback::Window
        );
        assert_eq!(
            fallback_for(true, Some(OsStr::new("wayland")), true, true),
            Fallback::Notifications
        );
        assert_eq!(
            fallback_for(true, None, true, false),
            Fallback::Notifications
        );
        // XWayland with a Wayland socket but an X display and no session type: a window works.
        assert_eq!(fallback_for(true, None, true, true), Fallback::Window);
    }
}
