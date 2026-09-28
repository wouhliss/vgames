//! Running games: spawn, track the whole process tree without polling, and
//! re-attach after a launcher restart (A2-T09).
//!
//! Every call here blocks. [`RunningGame::wait`] belongs on a dedicated thread
//! and returns early when its [`WaitCanceller`] fires, leaving the game running.
//! Games keep running when the launcher exits.

use serde::{Deserialize, Serialize};

use super::PreparedLaunch;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as sys;

#[cfg(not(target_os = "linux"))]
mod unsupported;
#[cfg(not(target_os = "linux"))]
use unsupported as sys;

/// What the launcher persists to find a game again after a restart. The start
/// time guards against a reused pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    /// Platform start time of the process (Linux: clock ticks since boot).
    pub start_time: u64,
}

/// How the tracked game ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameExit {
    /// The launched process exited with this code.
    Code(i32),
    /// The launched process was killed by this signal.
    Signal(i32),
    /// Re-attached after a restart: the launcher is not the parent and cannot read the status.
    Unknown,
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("cannot start the game")]
    Spawn(#[source] std::io::Error),
    #[error("cannot track the game process")]
    Track(#[source] std::io::Error),
    #[error("process tracking is not available on this platform yet")]
    Unsupported,
}

/// A game process tree: the launched process and everything it starts in its
/// process group.
pub struct RunningGame {
    inner: sys::Game,
}

/// Wakes a blocked [`RunningGame::wait`] without touching the game.
#[derive(Clone)]
pub struct WaitCanceller {
    inner: sys::Canceller,
}

/// Signals the game's process tree from any thread.
#[derive(Clone)]
pub struct TreeKiller {
    inner: sys::Killer,
}

impl RunningGame {
    /// Starts the prepared command in a new process group.
    pub fn spawn(launch: &PreparedLaunch) -> Result<Self, ProcessError> {
        sys::Game::spawn(launch.command()).map(|inner| Self { inner })
    }

    /// Finds a game started by an earlier launcher run. `None` when it has
    /// exited or its pid now belongs to another process.
    pub fn reattach(identity: ProcessIdentity) -> Result<Option<Self>, ProcessError> {
        Ok(sys::Game::reattach(identity)?.map(|inner| Self { inner }))
    }

    pub fn identity(&self) -> ProcessIdentity {
        self.inner.identity()
    }

    pub fn canceller(&self) -> WaitCanceller {
        WaitCanceller {
            inner: self.inner.canceller(),
        }
    }

    pub fn killer(&self) -> TreeKiller {
        TreeKiller {
            inner: self.inner.killer(),
        }
    }

    /// Blocks until every process of the tree has exited (`Some`) or the
    /// canceller fired (`None`).
    pub fn wait(self) -> Result<Option<GameExit>, ProcessError> {
        self.inner.wait()
    }
}

impl WaitCanceller {
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}

impl TreeKiller {
    /// Asks every process of the tree to exit (`force`: kill it).
    pub fn terminate(&self, force: bool) -> Result<(), ProcessError> {
        self.inner.terminate(force)
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;
