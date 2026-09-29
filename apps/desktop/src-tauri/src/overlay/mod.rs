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
//! Launch integration (for Agent 2's launch plan, A2-T09): call
//! [`OverlayService::prepare_launch`] before starting a game and merge the returned variables
//! into its environment; `GameStopped` on the bus ends the broker and feeds the valve.

pub mod broker;
pub mod commands;
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
use crate::social::service::{SocialService, SocialSettingsKey};
use broker::{Broker, BrokerEvent};
use hub::Hub;
use model::{OverlayAction, PackageOverlay};

/// Actions waiting to run (renderer floods are dropped beyond this).
const ACTION_QUEUE: usize = 32;

type Callback = Box<dyn Fn() + Send + Sync>;
type PackageCallback = Box<dyn Fn(PackageRef) + Send + Sync>;

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
            }),
        };
        tokio::spawn(service.clone().action_loop(rx, shutdown.clone()));
        tokio::spawn(service.clone().bus_loop(bus.subscribe(), shutdown));
        service
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.inner.hub
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
        tokio::spawn(log_broker(erx));
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

    fn toggle_panel(&self) {
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
                    let had = lock(&self.inner.sessions)
                        .remove(&stopped.package)
                        .is_some();
                    if lock(&self.inner.sessions).is_empty() {
                        self.set_panel(false);
                    }
                    if had {
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

async fn log_broker(mut rx: mpsc::UnboundedReceiver<BrokerEvent>) {
    while let Some(e) = rx.recv().await {
        match e {
            BrokerEvent::Connected(kind) => tracing::info!(?kind, "overlay renderer connected"),
            BrokerEvent::Disconnected => tracing::info!("overlay renderer disconnected"),
            BrokerEvent::AuthFailed => tracing::warn!("overlay connection refused"),
            BrokerEvent::Stopped => tracing::warn!("overlay broker stopped"),
        }
    }
}
