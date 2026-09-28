//! Game launch (A2-T09): turns a verified install plus its signed manifest into
//! an explicit program, argv, working directory and environment.
//!
//! Security rules (01-security §7): never a shell, the executable and working
//! directory are resolved inside the install with no symlink escape, the
//! environment is an allowlist of host variables plus validated manifest and
//! compat-profile variables, and invite join secrets are substituted only as a
//! whole argument after strict validation (05-social §5).
//!
//! [`target::resolve`] picks the launch target; a [`plan::LaunchPlan`]
//! (`Native`, `Proton` or `Wine`) turns it into a [`plan::PreparedLaunch`].

pub mod plan;
pub mod target;

pub use plan::{LaunchPlan, PreparedLaunch, ProtonPlan, WinePlan};
pub use target::{JoinSecret, ResolvedTarget, TargetChoice};

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("the package has no launch targets")]
    NoTargets,
    #[error("launch target {0:?} does not exist")]
    UnknownTarget(String),
    #[error("{what} {path} is missing or not inside the install")]
    OutsideInstall { what: &'static str, path: PathBuf },
    #[error("variable {0:?} may not be set for a game")]
    EnvKey(String),
}
