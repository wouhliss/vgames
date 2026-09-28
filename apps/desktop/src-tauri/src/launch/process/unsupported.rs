//! Placeholder until the Windows Job Object and macOS kqueue trackers land (A2-T09).

use std::process::Command;

use super::{GameExit, ProcessError, ProcessIdentity};

pub(super) enum Game {}

#[derive(Clone)]
pub(super) struct Canceller;

#[derive(Clone)]
pub(super) struct Killer;

impl Game {
    pub(super) fn spawn(_: Command) -> Result<Self, ProcessError> {
        Err(ProcessError::Unsupported)
    }

    pub(super) fn reattach(_: ProcessIdentity) -> Result<Option<Self>, ProcessError> {
        Err(ProcessError::Unsupported)
    }

    pub(super) fn identity(&self) -> ProcessIdentity {
        match *self {}
    }

    pub(super) fn canceller(&self) -> Canceller {
        match *self {}
    }

    pub(super) fn killer(&self) -> Killer {
        match *self {}
    }

    pub(super) fn wait(self) -> Result<Option<GameExit>, ProcessError> {
        match self {}
    }
}

impl Canceller {
    pub(super) fn cancel(&self) {}
}

impl Killer {
    pub(super) fn terminate(&self, _: bool) -> Result<(), ProcessError> {
        Err(ProcessError::Unsupported)
    }
}
