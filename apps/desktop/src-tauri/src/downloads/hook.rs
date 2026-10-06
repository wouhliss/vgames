//! The "priority files" hook (built in for GAME-06): fetch and verify some
//! files of a package first (the launch target), inspect them, and stop the
//! install before any further pack byte is requested if they cannot run here.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use vgames_core::manifest::Manifest;

use crate::catalog::CompatBlocker;
use crate::events::PackageRef;

pub type CheckFuture<'a> = Pin<Box<dyn Future<Output = Result<(), CompatBlocker>> + Send + 'a>>;

pub trait PriorityHook: Send + Sync + 'static {
    /// Manifest paths to fetch before anything else (an empty list skips the
    /// step). `manifest` passed signature verification.
    fn priority_files(&self, package: PackageRef, manifest: &Manifest) -> Vec<String>;

    /// Inspects the priority files, each complete and verified against the
    /// signed manifest. `Err` stops the install: no further pack byte is
    /// requested and partial files are removed.
    fn check<'a>(&'a self, package: PackageRef, paths: &'a [PathBuf]) -> CheckFuture<'a>;
}

/// The default: nothing first, always continue.
pub struct NoPriority;

impl PriorityHook for NoPriority {
    fn priority_files(&self, _package: PackageRef, _manifest: &Manifest) -> Vec<String> {
        Vec::new()
    }

    fn check<'a>(&'a self, _package: PackageRef, _paths: &'a [PathBuf]) -> CheckFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}
