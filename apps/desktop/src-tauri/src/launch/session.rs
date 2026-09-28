//! Running games (A2-T09): one session per package, `GameStarted` and
//! `GameStopped` on the event bus, playtime accounting, and re-attaching after
//! a launcher restart.
//!
//! A session is recorded in `<install>/.vgames/session.json` while the game
//! runs, so the next launcher run finds it with [`GameSessions::reattach`] and
//! credits the whole session. Each wait runs on its own thread (blocked in the
//! OS, no timers) and hands its result to an async task.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use uuid::Uuid;
use vgames_transfer::fsutil;
use vgames_transfer::install::META_DIR;

use super::PreparedLaunch;
use super::process::{self, ProcessError, ProcessIdentity, RunningGame, TreeKiller, WaitCanceller};
use crate::db::{self, Db};
use crate::events::{AppEvent, EventBus, GameExit, GameStarted, GameStopped, PackageRef};

pub const SESSION_FILE: &str = "session.json";
const SESSION_FORMAT: &str = "vgames.session/1";
const MAX_SESSION_BYTES: u64 = 4096;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SessionRecord {
    format: String,
    server_id: Uuid,
    package_id: Uuid,
    pid: u32,
    start_time: u64,
    /// Unix seconds.
    started_at: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("the game is already running")]
    AlreadyRunning,
    #[error("the game is not running")]
    NotRunning,
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error("cannot record the game session")]
    Record(#[source] io::Error),
}

enum Slot {
    /// Reserved while the process is being started or re-attached.
    Starting,
    Running {
        killer: TreeKiller,
        canceller: WaitCanceller,
        stopped_by_user: Arc<AtomicBool>,
    },
}

/// Cloneable handle to the running games.
#[derive(Clone)]
pub struct GameSessions {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    bus: EventBus,
    slots: Mutex<HashMap<PackageRef, Slot>>,
}

impl GameSessions {
    pub fn new(db: Db, bus: EventBus) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                bus,
                slots: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn is_running(&self, package: PackageRef) -> bool {
        self.slots().contains_key(&package)
    }

    /// Spawns a checked, prepared launch (`prelaunch` then `LaunchPlan::prepare`)
    /// and tracks it. Returns the pid.
    pub async fn start(
        &self,
        package: PackageRef,
        install_root: PathBuf,
        launch: PreparedLaunch,
    ) -> Result<u32, SessionError> {
        self.reserve(package)?;
        let started = tauri::async_runtime::spawn_blocking(move || {
            let game = RunningGame::spawn(&launch)?;
            let identity = game.identity();
            let record = SessionRecord {
                format: SESSION_FORMAT.into(),
                server_id: package.server_id,
                package_id: package.package_id,
                pid: identity.pid,
                start_time: identity.start_time,
                started_at: db::now_unix(),
            };
            // The game runs either way; without the record only a launcher
            // restart during this session loses its playtime.
            if let Err(error) = write_record(&install_root, &record) {
                tracing::warn!(%error, "cannot record the game session");
            }
            Ok::<_, SessionError>((game, record, install_root))
        })
        .await;
        match started {
            Ok(Ok((game, record, root))) => {
                let pid = record.pid;
                self.track(package, root, game, record.started_at);
                Ok(pid)
            }
            Ok(Err(error)) => {
                self.slots().remove(&package);
                Err(error)
            }
            Err(join) => {
                self.slots().remove(&package);
                Err(SessionError::Record(io::Error::other(join)))
            }
        }
    }

    /// Finds a game left running by an earlier launcher run. Returns whether
    /// it is still running; a stale record is removed.
    pub async fn reattach(
        &self,
        package: PackageRef,
        install_root: PathBuf,
    ) -> Result<bool, SessionError> {
        self.reserve(package)?;
        let found = tauri::async_runtime::spawn_blocking(move || {
            let Some(record) = read_record(&install_root) else {
                return Ok(None);
            };
            if record.server_id != package.server_id || record.package_id != package.package_id {
                remove_record(&install_root);
                return Ok(None);
            }
            let identity = ProcessIdentity {
                pid: record.pid,
                start_time: record.start_time,
            };
            match RunningGame::reattach(identity)? {
                Some(game) => Ok(Some((game, record.started_at, install_root))),
                None => {
                    // It ended while the launcher was closed; the end time is
                    // unknown, so that session is not credited.
                    remove_record(&install_root);
                    Ok(None)
                }
            }
        })
        .await
        .unwrap_or_else(|join| Err(SessionError::Record(io::Error::other(join))));
        match found {
            Ok(Some((game, started_at, root))) => {
                self.track(package, root, game, started_at);
                Ok(true)
            }
            other => {
                self.slots().remove(&package);
                other.map(|_| false)
            }
        }
    }

    /// Ends the game's whole process tree. The UI confirms first.
    pub fn stop(&self, package: PackageRef, force: bool) -> Result<(), SessionError> {
        let slots = self.slots();
        match slots.get(&package) {
            Some(Slot::Running {
                killer,
                stopped_by_user,
                ..
            }) => {
                stopped_by_user.store(true, Ordering::Release);
                Ok(killer.terminate(force)?)
            }
            _ => Err(SessionError::NotRunning),
        }
    }

    /// Launcher exit: stop waiting, leave the games running and their
    /// records in place for the next run.
    pub fn detach_all(&self) {
        for slot in self.slots().values() {
            if let Slot::Running { canceller, .. } = slot {
                canceller.cancel();
            }
        }
    }

    fn reserve(&self, package: PackageRef) -> Result<(), SessionError> {
        let mut slots = self.slots();
        if slots.contains_key(&package) {
            return Err(SessionError::AlreadyRunning);
        }
        slots.insert(package, Slot::Starting);
        Ok(())
    }

    fn track(&self, package: PackageRef, root: PathBuf, game: RunningGame, started_at: i64) {
        let pid = game.identity().pid;
        let stopped_by_user = Arc::new(AtomicBool::new(false));
        self.slots().insert(
            package,
            Slot::Running {
                killer: game.killer(),
                canceller: game.canceller(),
                stopped_by_user: stopped_by_user.clone(),
            },
        );
        let (done, result) = oneshot::channel();
        let waiter = std::thread::Builder::new()
            .name("vgames-game-wait".into())
            .spawn(move || {
                let _ = done.send(game.wait());
            });
        if let Err(error) = waiter {
            tracing::error!(%error, "cannot start the game tracker");
        }
        self.inner
            .bus
            .publish(AppEvent::GameStarted(GameStarted { package, pid }));

        let sessions = self.clone();
        tauri::async_runtime::spawn(async move {
            let outcome = result.await;
            sessions.slots().remove(&package);
            let exit = match outcome {
                Ok(Ok(Some(exit))) => exit,
                // Detached on launcher exit: the record stays for the next run.
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    tracing::error!(error = %crate::error::DisplayChain(&error), "game tracking failed");
                    process::GameExit::Unknown
                }
                // The tracker never ran (no thread): the game may still be
                // running, so keep its record for the next launcher run.
                Err(_) => return,
            };
            sessions
                .finish(
                    package,
                    root,
                    exit,
                    started_at,
                    stopped_by_user.load(Ordering::Acquire),
                )
                .await;
        });
    }

    async fn finish(
        &self,
        package: PackageRef,
        root: PathBuf,
        exit: process::GameExit,
        started_at: i64,
        stopped_by_user: bool,
    ) {
        let ended_at = db::now_unix();
        let session_seconds =
            u32::try_from(ended_at.saturating_sub(started_at).max(0)).unwrap_or(u32::MAX);
        match db::installs::record_playtime(&self.inner.db, package, session_seconds, ended_at)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                tracing::info!("playtime not recorded: the package is no longer installed")
            }
            Err(error) => {
                tracing::warn!(error = %crate::error::DisplayChain(&error), "cannot record playtime")
            }
        }
        if let Err(error) = tauri::async_runtime::spawn_blocking(move || remove_record(&root)).await
        {
            tracing::warn!(%error, "cannot remove the session record");
        }
        let code = match exit {
            process::GameExit::Code(code) => Some(code),
            process::GameExit::Signal(_) | process::GameExit::Unknown => None,
        };
        self.inner.bus.publish(AppEvent::GameStopped(GameStopped {
            package,
            exit: GameExit {
                code,
                stopped_by_user,
                session_seconds,
            },
        }));
    }

    fn slots(&self) -> std::sync::MutexGuard<'_, HashMap<PackageRef, Slot>> {
        self.inner
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Startup: re-attaches every game an earlier launcher run left running.
pub fn spawn_reattach(db: Db, games: GameSessions) {
    tauri::async_runtime::spawn(async move {
        let roots = match db::installs::roots(&db).await {
            Ok(roots) => roots,
            Err(error) => {
                tracing::warn!(error = %crate::error::DisplayChain(&error), "cannot list installs");
                return;
            }
        };
        for (package, root) in roots {
            match games.reattach(package, root).await {
                Ok(true) => tracing::info!("re-attached to a running game"),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(error = %crate::error::DisplayChain(&error), "cannot re-attach a game");
                }
            }
        }
    });
}

fn record_path(root: &Path) -> PathBuf {
    root.join(META_DIR).join(SESSION_FILE)
}

fn write_record(root: &Path, record: &SessionRecord) -> io::Result<()> {
    let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
    fsutil::atomic_write(&record_path(root), &bytes)
}

/// `None` for a missing, oversized, linked or malformed record.
fn read_record(root: &Path) -> Option<SessionRecord> {
    let path = record_path(root);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_SESSION_BYTES {
        return None;
    }
    let record: SessionRecord = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    (record.format == SESSION_FORMAT).then_some(record)
}

fn remove_record(root: &Path) {
    match std::fs::remove_file(record_path(root)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(%error, "cannot remove the session record"),
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;
