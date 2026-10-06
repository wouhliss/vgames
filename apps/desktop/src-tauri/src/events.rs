//! The launcher's internal event bus and the typed events the UI receives.
//!
//! Modules publish [`AppEvent`]s on the [`EventBus`] (a `tokio::sync::broadcast`
//! channel) and other modules subscribe: presence and invites (Agent 4) follow
//! game and install events, the overlay follows the controller Guide hold, and
//! so on. A bridge task forwards the UI-relevant ones to the WebView as
//! tauri-specta events, so the UI gets the same typed payloads.
//!
//! Subscribers must tolerate `RecvError::Lagged` (a slow subscriber skips old
//! events; state is always re-readable through commands).

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Runtime};
use tauri_specta::Event as _;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Capacity per subscriber before it starts lagging.
const BUS_CAPACITY: usize = 256;

/// A package on a specific server (package ids are only unique per server).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
pub struct PackageRef {
    pub server_id: Uuid,
    pub package_id: Uuid,
}

/// A game process tree was spawned.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct GameStarted {
    pub package: PackageRef,
    /// OS process id of the tracked root process.
    pub pid: u32,
}

/// How a game session ended.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct GameExit {
    /// Exit code of the root process, when the OS reports one.
    pub code: Option<i32>,
    /// Whether the launcher stopped the game at the user's request.
    pub stopped_by_user: bool,
    /// Duration of this session in seconds.
    pub session_seconds: u32,
}

/// The whole process tree of a game exited.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct GameStopped {
    pub package: PackageRef,
    pub exit: GameExit,
}

/// Phase of an install, update or repair (02-package-format §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum InstallPhase {
    Queued,
    VerifyingManifest,
    Allocating,
    Downloading,
    Finalizing,
    Verifying,
    Paused,
}

/// Progress of the active install. Emitted at most 4 times per second.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct InstallProgress {
    pub package: PackageRef,
    pub phase: InstallPhase,
    /// Byte counts are below 2^53, so they are safe as JS numbers.
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Exponentially weighted (5 s) transfer rate.
    pub bytes_per_second: u64,
    pub eta_seconds: Option<u32>,
    pub connections: u32,
}

/// How an install, update or repair ended.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstallOutcome {
    Installed,
    Cancelled { kept_partial: bool },
    Failed { code: String, message: String },
}

/// An install, update or repair finished (successfully or not).
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct InstallFinished {
    pub package: PackageRef,
    pub outcome: InstallOutcome,
}

/// The active server changed (`None`: no server is active). Realtime
/// connections to the previous server must be dropped.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct ServerSwitched {
    pub server_id: Option<Uuid>,
}

/// The list of servers or one of their accounts changed; re-read `servers_list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct ServersChanged {}

/// Whether the launcher could reach a server on its last attempt.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct ConnectivityChanged {
    pub server_id: Uuid,
    pub online: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TrustProblemKind {
    FingerprintMismatch,
}

/// A server presented a root key other than the pinned one. It is blocked
/// until it presents the pinned key again; there is no "continue anyway".
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct TrustProblem {
    pub server_id: Uuid,
    pub kind: TrustProblemKind,
    pub server_name: String,
    pub pinned_fingerprint: String,
    pub presented_fingerprint: String,
}

/// A `vgames://server/add` link was opened; the UI starts onboarding with
/// these values (then calls `server_preview(url, fingerprint)`).
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct ServerAddRequested {
    pub url: String,
    pub fingerprint: String,
}

/// A sign-in finished (deep-link callback, pasted code, or cancel).
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct AuthFinished {
    pub flow_id: Uuid,
    pub outcome: crate::servers::auth::AuthOutcome,
}

/// Controller family, as classified by the input thread (07-controllers §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ControllerKind {
    Xinput,
    Dualshock4,
    Dualsense,
    SwitchPro,
    Generic,
}

/// What happened to a controller.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControllerChange {
    Connected {
        controller: ControllerKind,
        name: String,
    },
    Disconnected,
    /// Guide/PS held for 1 s (opens the in-game overlay, 05-social §6).
    GuideHeld,
}

/// A controller event from the input thread.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct ControllerEvent {
    /// SDL joystick instance id; stable while the pad stays connected.
    pub instance_id: u32,
    pub change: ControllerChange,
}

/// Installed packages changed (state, favorites, collections, versions); re-read
/// `installs_list` (INS-04).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct InstallsChanged {}

/// Libraries were added, removed or a new default chosen; re-read
/// `libraries_list` (INS-04).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct LibrariesChanged {}

/// Collections changed; re-read `collections_list` (INS-04).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct CollectionsChanged {}

/// Everything published on the internal bus.
#[derive(Debug, Clone)]
pub enum AppEvent {
    GameStarted(GameStarted),
    GameStopped(GameStopped),
    InstallProgress(InstallProgress),
    InstallFinished(InstallFinished),
    ServerSwitched(ServerSwitched),
    Controller(ControllerEvent),
    ServersChanged(ServersChanged),
    ConnectivityChanged(ConnectivityChanged),
    TrustProblem(TrustProblem),
    ServerAddRequested(ServerAddRequested),
    AuthFinished(AuthFinished),
    /// A `vgames://` URL arrived from the OS (first launch, second instance or
    /// open-url event). Untrusted: the deep-link router parses it (A2-T10).
    DeepLinkReceived {
        url: String,
    },
    /// The install queue or its history changed (INS-03).
    DownloadsChanged(crate::downloads::DownloadsChanged),
    InstallsChanged(InstallsChanged),
    CollectionsChanged(CollectionsChanged),
    LibrariesChanged(LibrariesChanged),
}

/// Cloneable handle to the internal broadcast bus.
#[derive(Debug, Clone)]
pub struct EventBus {
    sender: broadcast::Sender<AppEvent>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(BUS_CAPACITY);
        Self { sender }
    }

    /// Publishes an event. Having no subscriber is not an error.
    pub fn publish(&self, event: AppEvent) {
        let _ = self.sender.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.sender.subscribe()
    }
}

/// Forwards UI-relevant bus events to the WebView until `shutdown` fires.
/// Event-driven: the task sleeps in `recv()` while nothing happens.
pub fn spawn_ui_bridge<R: Runtime>(app: AppHandle<R>, bus: &EventBus, shutdown: CancellationToken) {
    let mut receiver = bus.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            let event = tokio::select! {
                () = shutdown.cancelled() => break,
                event = receiver.recv() => event,
            };
            let result = match event {
                Ok(AppEvent::GameStarted(e)) => e.emit(&app),
                Ok(AppEvent::GameStopped(e)) => e.emit(&app),
                Ok(AppEvent::InstallProgress(e)) => e.emit(&app),
                Ok(AppEvent::InstallFinished(e)) => e.emit(&app),
                Ok(AppEvent::ServerSwitched(e)) => e.emit(&app),
                Ok(AppEvent::Controller(e)) => e.emit(&app),
                Ok(AppEvent::ServersChanged(e)) => e.emit(&app),
                Ok(AppEvent::ConnectivityChanged(e)) => e.emit(&app),
                Ok(AppEvent::TrustProblem(e)) => e.emit(&app),
                Ok(AppEvent::ServerAddRequested(e)) => e.emit(&app),
                Ok(AppEvent::AuthFinished(e)) => e.emit(&app),
                Ok(AppEvent::DeepLinkReceived { .. }) => Ok(()),
                Ok(AppEvent::DownloadsChanged(e)) => e.emit(&app),
                Ok(AppEvent::InstallsChanged(e)) => e.emit(&app),
                Ok(AppEvent::CollectionsChanged(e)) => e.emit(&app),
                Ok(AppEvent::LibrariesChanged(e)) => e.emit(&app),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "UI event bridge lagged");
                    Ok(())
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            if let Err(error) = result {
                tracing::warn!(%error, "failed to emit a UI event");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package() -> PackageRef {
        PackageRef {
            server_id: Uuid::nil(),
            package_id: Uuid::max(),
        }
    }

    #[tokio::test]
    async fn every_subscriber_sees_every_event() {
        let bus = EventBus::new();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        bus.publish(AppEvent::GameStarted(GameStarted {
            package: package(),
            pid: 42,
        }));
        for rx in [&mut a, &mut b] {
            match rx.recv().await.unwrap() {
                AppEvent::GameStarted(e) => assert_eq!(e.pid, 42),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn publishing_without_subscribers_is_fine() {
        EventBus::new().publish(AppEvent::ServerSwitched(ServerSwitched { server_id: None }));
    }

    #[tokio::test]
    async fn slow_subscribers_lag_instead_of_blocking() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        for _ in 0..(BUS_CAPACITY + 10) {
            bus.publish(AppEvent::ServerSwitched(ServerSwitched { server_id: None }));
        }
        assert!(matches!(
            rx.recv().await,
            Err(broadcast::error::RecvError::Lagged(10))
        ));
    }

    #[test]
    fn payloads_serialize_as_documented() {
        let finished = InstallFinished {
            package: package(),
            outcome: InstallOutcome::Cancelled { kept_partial: true },
        };
        let json = serde_json::to_value(&finished).unwrap();
        assert_eq!(json["outcome"]["kind"], "cancelled");
        assert_eq!(json["outcome"]["kept_partial"], true);
        let event = ControllerEvent {
            instance_id: 3,
            change: ControllerChange::Connected {
                controller: ControllerKind::SwitchPro,
                name: "Pro Controller".into(),
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["change"]["controller"], "switch_pro");
    }
}
