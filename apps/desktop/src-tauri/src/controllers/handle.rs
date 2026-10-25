//! Asynchronous startup and shutdown without blocking the WebView thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use specta::Type;
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::routing::{ConnectedPad, Output, Tester};
use super::service::{Service, ServiceError};

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControllerAvailability {
    Ready,
    Unavailable { detail: String },
}
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ControllerList {
    pub state: ControllerAvailability,
    pub pads: Vec<ConnectedPad>,
}
enum State {
    Unavailable(String),
    Ready(Service),
}
#[derive(Clone)]
pub struct Controllers {
    state: Arc<Mutex<State>>,
    focused: Arc<AtomicBool>,
}
impl Default for Controllers {
    fn default() -> Self {
        Self {
            focused: Arc::new(AtomicBool::new(false)),
            state: Arc::new(Mutex::new(State::Unavailable(
                "Controller input is starting.".into(),
            ))),
        }
    }
}
impl Controllers {
    pub fn start(
        &self,
        runtime: Handle,
        emit: Arc<dyn Fn(Output) + Send + Sync>,
        shutdown: CancellationToken,
    ) {
        let state = self.state.clone();
        let focused = self.focused.clone();
        let worker_runtime = runtime.clone();
        runtime.spawn(async move {
            let startup_shutdown = shutdown.clone();
            let startup_emit = emit.clone();
            let startup_state = state.clone();
            let initialized = tokio::task::spawn_blocking(move || {
                let service = Service::start(worker_runtime, startup_emit);
                if startup_shutdown.is_cancelled() {
                    // Dropping a successful initialization joins its worker here,
                    // on the blocking pool, even if shutdown won the startup race.
                    drop(service);
                    return;
                }
                if let Ok(mut state) = startup_state.lock() {
                    *state = match service {
                        Ok(service) => {
                            let _ = service.navigation_enabled(focused.load(Ordering::Acquire));
                            State::Ready(service)
                        }
                        Err(error) => State::Unavailable(error.to_string()),
                    };
                }
            })
            .await;
            if let Err(error) = initialized {
                tracing::error!(%error, "controller initialization task failed");
            }
            emit(Output::PadsChanged);
            shutdown.cancelled().await;
            let stopped = tokio::task::spawn_blocking(move || {
                let service = state.lock().ok().map(|mut state| {
                    std::mem::replace(
                        &mut *state,
                        State::Unavailable("Controller input has stopped.".into()),
                    )
                });
                // Joining SDL never holds the IPC state lock.
                drop(service);
            })
            .await;
            if let Err(error) = stopped {
                tracing::error!(%error, "controller shutdown task failed");
            }
        });
    }
    pub fn list(&self) -> ControllerList {
        match self.state.lock() {
            Ok(state) => match &*state {
                State::Ready(service) => match service.list() {
                    Ok(pads) => ControllerList {
                        state: ControllerAvailability::Ready,
                        pads,
                    },
                    Err(error) => unavailable(error.to_string()),
                },
                State::Unavailable(detail) => unavailable(detail.clone()),
            },
            Err(_) => unavailable("Controller state is unavailable.".into()),
        }
    }
    pub fn tester_start(&self, id: u32) -> Result<Tester, ServiceError> {
        let state = self.state.lock().map_err(|_| ServiceError::State)?;
        match &*state {
            State::Ready(service) => service.tester_start(id),
            State::Unavailable(detail) => Err(ServiceError::Unavailable(detail.clone())),
        }
    }
    pub fn tester_stop(&self, id: Uuid) -> Result<(), ServiceError> {
        let state = self.state.lock().map_err(|_| ServiceError::State)?;
        match &*state {
            State::Ready(service) => service.tester_stop(id),
            State::Unavailable(_) => Ok(()),
        }
    }
    pub fn navigation_enabled(&self, enabled: bool) {
        self.focused.store(enabled, Ordering::Release);
        if let Ok(state) = self.state.lock()
            && let State::Ready(service) = &*state
            && let Err(error) = service.navigation_enabled(enabled)
        {
            tracing::warn!(%error, "cannot change controller navigation focus");
        }
    }
}
fn unavailable(detail: String) -> ControllerList {
    ControllerList {
        state: ControllerAvailability::Unavailable { detail },
        pads: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_input_is_visible_and_closing_a_tester_is_idempotent() {
        let controllers = Controllers::default();
        assert!(matches!(
            controllers.list().state,
            ControllerAvailability::Unavailable { .. }
        ));
        assert!(controllers.list().pads.is_empty());
        assert!(controllers.tester_start(99).is_err());
        controllers.tester_stop(Uuid::new_v4()).unwrap();
    }
}
