//! Launch target selection and confinement to the install directory.
//! Filesystem calls block: run [`resolve`] on a blocking pool.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use vgames_core::manifest::{LaunchTarget, Manifest};

use super::TargetError;

/// The placeholder a manifest's `multiplayer.join.args` may contain as a whole argument.
const JOIN_PLACEHOLDER: &str = "{join_secret}";
const MAX_JOIN_SECRET: usize = 256;

/// An invite join secret matching `^[A-Za-z0-9._:\-\[\]]{1,256}$` (05-social §5).
#[derive(Clone, PartialEq, Eq)]
pub struct JoinSecret(String);

impl JoinSecret {
    pub fn parse(raw: &str) -> Option<Self> {
        let valid = (1..=MAX_JOIN_SECRET).contains(&raw.len())
            && raw.bytes().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'-' | b'[' | b']')
            });
        valid.then(|| Self(raw.to_owned()))
    }
}

// Secrets never reach logs.
impl std::fmt::Debug for JoinSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JoinSecret(..)")
    }
}

/// Which target to start. URLs never choose arguments (01-security §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetChoice {
    /// `launch.default`.
    Default,
    /// A target the player picked by id.
    Target(String),
    /// The manifest's `multiplayer.join` target with the secret substituted.
    Join(JoinSecret),
}

impl TargetChoice {
    /// Choice for an accepted invite: an absent, empty or invalid secret means
    /// a normal launch (05-social §5).
    pub fn for_invite(secret: Option<&str>) -> Self {
        secret
            .and_then(JoinSecret::parse)
            .map_or(Self::Default, Self::Join)
    }
}

/// A target with absolute, canonical paths inside the install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub id: String,
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    /// Manifest `env` (validated by `vgames_core::manifest`).
    pub env: BTreeMap<String, String>,
}

/// Selects the target and resolves its executable and working directory under
/// `install_root`. `manifest` must come from `parse_and_validate` of the
/// install's verified manifest bytes.
pub fn resolve(
    install_root: &Path,
    manifest: &Manifest,
    choice: &TargetChoice,
) -> Result<ResolvedTarget, TargetError> {
    let launch = manifest.launch.as_ref().ok_or(TargetError::NoTargets)?;
    let find = |id: &str| {
        launch
            .targets
            .iter()
            .find(|t| t.id == id)
            .ok_or_else(|| TargetError::UnknownTarget(id.to_owned()))
    };
    let join = match choice {
        TargetChoice::Join(secret) => manifest.multiplayer.as_ref().map(|m| (&m.join, secret)),
        _ => None,
    };
    let (target, extra) = match (choice, join) {
        (TargetChoice::Target(id), _) => (find(id)?, Vec::new()),
        (_, Some((join, secret))) => {
            let extra = join
                .args
                .iter()
                .map(|arg| {
                    if arg == JOIN_PLACEHOLDER {
                        secret.0.clone()
                    } else {
                        arg.clone()
                    }
                })
                .collect();
            (find(&join.target)?, extra)
        }
        // Default, or no `multiplayer.join` in the manifest: a normal launch.
        _ => (find(&launch.default)?, Vec::new()),
    };
    let mut args = target.args.clone();
    args.extend(extra);
    confine(install_root, target, args)
}

fn confine(
    install_root: &Path,
    target: &LaunchTarget,
    args: Vec<String>,
) -> Result<ResolvedTarget, TargetError> {
    let root = fs::canonicalize(install_root).map_err(|_| TargetError::OutsideInstall {
        what: "install",
        path: install_root.to_owned(),
    })?;
    let executable = inside(&root, &target.executable, "executable")?;
    if !fs::metadata(&executable).is_ok_and(|m| m.is_file()) {
        return Err(TargetError::OutsideInstall {
            what: "executable",
            path: executable,
        });
    }
    let working_dir = match &target.working_dir {
        Some(dir) => inside(&root, dir, "working directory")?,
        None => executable
            .parent()
            .map_or_else(|| root.clone(), Path::to_path_buf),
    };
    if !fs::metadata(&working_dir).is_ok_and(|m| m.is_dir()) {
        return Err(TargetError::OutsideInstall {
            what: "working directory",
            path: working_dir,
        });
    }
    Ok(ResolvedTarget {
        id: target.id.clone(),
        executable,
        args,
        working_dir,
        env: target.env.clone(),
    })
}

/// Joins a validated package path (`/`-separated) to `root` and canonicalizes
/// it, refusing anything a symlink moved outside `root`.
fn inside(root: &Path, package_path: &str, what: &'static str) -> Result<PathBuf, TargetError> {
    let joined = package_path
        .split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part));
    match fs::canonicalize(&joined) {
        Ok(real) if real.starts_with(root) && real != root => Ok(real),
        _ => Err(TargetError::OutsideInstall { what, path: joined }),
    }
}

#[cfg(test)]
pub(crate) mod tests;
