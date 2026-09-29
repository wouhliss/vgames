//! Pre-launch checks (02 §11): the install is complete, its stored manifest is
//! signed by a key the cached trust bundle does not revoke, and the launch
//! executable hashes to the manifest. Results are cached by file stamps (size,
//! mtime and, on Unix, inode and ctime), so a repeat launch skips re-hashing.
//! Everything here blocks: run it on a blocking pool.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use vgames_core::manifest::Manifest;
use vgames_core::trust::TrustState;
use vgames_core::verify::{ExpectedRelease, VerifyError, VerifyMode};
use vgames_transfer::install::{
    self, InstallError, InstallState, MANIFEST_FILE, META_DIR, RECORD_FILE, SIGNATURE_FILE,
};

use super::TargetError;
use super::target::{self, ResolvedTarget, TargetChoice};

#[derive(Debug, thiserror::Error)]
pub enum PrelaunchError {
    #[error("the package is not fully installed")]
    NotInstalled,
    /// The signing key was revoked: the UI offers "Re-verify" (02 §9).
    #[error("the package's signing key was revoked; verify the package again")]
    Reverify,
    #[error("the installed package failed its integrity check: {0}")]
    Integrity(String),
    /// `path` is the executable's package path (`/`-separated).
    #[error("the game executable {path} was modified; repair the package")]
    ExecutableModified { path: String },
    #[error(transparent)]
    Launch(#[from] TargetError),
    #[error("cannot read {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// What a passed check hands to the launch.
#[derive(Debug, Clone)]
pub struct Checked {
    pub manifest: Arc<Manifest>,
    pub target: ResolvedTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    inode: (u64, u64),
    #[cfg(unix)]
    changed: (i64, i64),
}

struct VerifiedInstall {
    stamps: [Stamp; 3],
    trust: (uuid::Uuid, u64),
    expected: ExpectedRelease,
    manifest: Arc<Manifest>,
}

/// Cache of passed checks, per install directory and per executable.
#[derive(Default)]
pub struct Prelaunch {
    installs: Mutex<HashMap<PathBuf, Arc<VerifiedInstall>>>,
    executables: Mutex<HashMap<PathBuf, Stamp>>,
    #[cfg(test)]
    hashed: std::sync::atomic::AtomicUsize,
}

impl Prelaunch {
    pub fn check(
        &self,
        root: &Path,
        trust: &TrustState,
        expected: &ExpectedRelease,
        choice: &TargetChoice,
    ) -> Result<Checked, PrelaunchError> {
        let install = self.verified_install(root, trust, expected)?;
        let target = target::resolve(root, &install.manifest, choice)?;
        self.check_executable(&install.manifest, &target)?;
        Ok(Checked {
            manifest: install.manifest.clone(),
            target,
        })
    }

    /// Drops cached results for an install (after update, repair, move, uninstall).
    /// Blocks briefly (one `canonicalize`).
    pub fn forget(&self, root: &Path) {
        lock(&self.installs).remove(root);
        // Executables are cached under their canonical path (`\\?\` on
        // Windows, links resolved), which `root` may not be.
        let canonical = fs::canonicalize(root).ok();
        lock(&self.executables).retain(|path, _| {
            !path.starts_with(root) && canonical.as_ref().is_none_or(|c| !path.starts_with(c))
        });
    }

    /// Steps 1 and 2.
    fn verified_install(
        &self,
        root: &Path,
        trust: &TrustState,
        expected: &ExpectedRelease,
    ) -> Result<Arc<VerifiedInstall>, PrelaunchError> {
        let meta = root.join(META_DIR);
        let stamps = [
            stamp(&meta.join(RECORD_FILE))?,
            stamp(&meta.join(MANIFEST_FILE))?,
            stamp(&meta.join(SIGNATURE_FILE))?,
        ];
        let trust_id = (trust.server_id(), trust.version());
        if let Some(hit) = lock(&self.installs).get(root)
            && hit.stamps == stamps
            && hit.trust == trust_id
            && hit.expected == *expected
        {
            return Ok(hit.clone());
        }

        let record = install::read_record(root)
            .map_err(|e| from_install(e, root))?
            .ok_or(PrelaunchError::NotInstalled)?;
        if record.state != InstallState::Installed {
            return Err(PrelaunchError::NotInstalled);
        }
        let release = install::load_local_release(root, trust, expected, VerifyMode::Launch)
            .map_err(|e| from_install(e, root))?;
        if release.verified.digest.to_hex() != record.manifest_blake3 {
            return Err(PrelaunchError::Integrity(
                "the install record does not match its manifest".into(),
            ));
        }
        let verified = Arc::new(VerifiedInstall {
            stamps,
            trust: trust_id,
            expected: expected.clone(),
            manifest: Arc::new(release.verified.manifest),
        });
        lock(&self.installs).insert(root.to_owned(), verified.clone());
        Ok(verified)
    }

    /// Step 3.
    fn check_executable(
        &self,
        manifest: &Manifest,
        target: &ResolvedTarget,
    ) -> Result<(), PrelaunchError> {
        let relative = manifest
            .launch
            .as_ref()
            .and_then(|l| l.targets.iter().find(|t| t.id == target.id))
            .map(|t| t.executable.as_str())
            .ok_or_else(|| TargetError::UnknownTarget(target.id.clone()))?;
        let entry = manifest
            .files
            .iter()
            .find(|f| f.path == relative)
            .ok_or_else(|| PrelaunchError::Integrity("the executable is not listed".into()))?;
        let path = &target.executable;
        let before = stamp(path)?;
        if lock(&self.executables).get(path) == Some(&before) {
            return Ok(());
        }
        let io_error = |source| PrelaunchError::Io {
            path: path.clone(),
            source,
        };
        #[cfg(test)]
        self.hashed
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut hasher = blake3::Hasher::new();
        let file = fs::File::open(path).map_err(io_error)?;
        hasher.update_reader(file).map_err(io_error)?;
        // A file changed while hashing is not trusted either.
        if hasher.finalize().as_bytes() != entry.blake3.as_bytes() || stamp(path)? != before {
            lock(&self.executables).remove(path);
            return Err(PrelaunchError::ExecutableModified {
                path: relative.to_owned(),
            });
        }
        lock(&self.executables).insert(path.clone(), before);
        Ok(())
    }
}

fn from_install(error: InstallError, root: &Path) -> PrelaunchError {
    match error {
        InstallError::Verify(VerifyError::RevokedKey(_)) => PrelaunchError::Reverify,
        InstallError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound => {
            PrelaunchError::NotInstalled
        }
        InstallError::Io { path, source, .. } => PrelaunchError::Io { path, source },
        other => {
            tracing::warn!(root = %root.display(), error = %other, "pre-launch verification failed");
            PrelaunchError::Integrity(other.to_string())
        }
    }
}

fn stamp(path: &Path) -> Result<Stamp, PrelaunchError> {
    let metadata = fs::metadata(path).map_err(|source| match source.kind() {
        io::ErrorKind::NotFound if path.parent().is_some_and(|p| p.ends_with(META_DIR)) => {
            PrelaunchError::NotInstalled
        }
        _ => PrelaunchError::Io {
            path: path.to_owned(),
            source,
        },
    })?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(Stamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        inode: (metadata.dev(), metadata.ino()),
        #[cfg(unix)]
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    })
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // The maps hold only cache entries: a panic elsewhere cannot corrupt them.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
