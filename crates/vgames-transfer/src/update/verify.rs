//! Read-only, bounded verification of installed files against a signed release.
//! Damaged file indices can be passed to the repair planner; an unsafe path or
//! unreadable file is an error rather than silently treated as missing.

use std::fs;
use std::io::{self, Read};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use vgames_core::Digest;

use crate::fsutil::{Access, SafePathError, SafeRoot};
use crate::install::Release;

const READ_SIZE: usize = 256 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct VerifyOptions {
    /// Concurrent blocking readers, clamped to 1–32.
    pub workers: usize,
    /// Aggregate read budget across all workers. `None` means unlimited.
    pub bytes_per_second: Option<NonZeroU64>,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self {
            workers: 4,
            bytes_per_second: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    /// Indices into the signed manifest, in manifest order.
    pub damaged_files: Vec<u32>,
    pub bytes_hashed: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error(transparent)]
    UnsafePath(#[from] SafePathError),
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("verification was cancelled")]
    Cancelled,
    #[error("a verification worker stopped unexpectedly")]
    Worker,
    #[error("the release contains too many files")]
    TooManyFiles,
}

struct RateGate {
    next: Mutex<Instant>,
    rate: NonZeroU64,
}

impl RateGate {
    fn wait(&self, bytes: usize, cancel: &CancellationToken) -> Result<(), VerifyError> {
        let delay = Duration::from_secs_f64(bytes as f64 / self.rate.get() as f64);
        let deadline = {
            let mut next = self.next.lock().map_err(|_| VerifyError::Worker)?;
            let now = Instant::now();
            let start = (*next).max(now);
            *next = start.checked_add(delay).ok_or(VerifyError::Worker)?;
            start
        };
        loop {
            if cancel.is_cancelled() {
                return Err(VerifyError::Cancelled);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            std::thread::sleep(remaining.min(Duration::from_millis(50)));
        }
    }
}

/// The release must already have passed `verify_manifest` under current trust.
/// File reads run on bounded blocking workers, never on the async runtime.
pub async fn verify_install(
    root: &Path,
    release: Arc<Release>,
    options: VerifyOptions,
    cancel: CancellationToken,
) -> Result<VerifyReport, VerifyError> {
    let root = root.to_owned();
    let safe = Arc::new(
        tokio::task::spawn_blocking(move || SafeRoot::open(&root))
            .await
            .map_err(|_| VerifyError::Worker)??,
    );
    let cancel = cancel.child_token();
    let gate = options.bytes_per_second.map(|rate| {
        Arc::new(RateGate {
            next: Mutex::new(Instant::now()),
            rate,
        })
    });
    let mut tasks = JoinSet::new();
    let mut damaged_files = Vec::new();
    let mut bytes_hashed = 0u64;
    let mut next = 0usize;
    let count = release.manifest().files.len();
    if count > u32::MAX as usize {
        return Err(VerifyError::TooManyFiles);
    }
    let workers = options.workers.clamp(1, 32);
    while next < count || !tasks.is_empty() {
        while next < count && tasks.len() < workers {
            let file = release
                .manifest()
                .files
                .get(next)
                .ok_or(VerifyError::TooManyFiles)?
                .clone();
            let index = u32::try_from(next).map_err(|_| VerifyError::TooManyFiles)?;
            let safe = Arc::clone(&safe);
            let gate = gate.clone();
            let cancel = cancel.clone();
            tasks
                .spawn_blocking(move || verify_file(&safe, index, &file, gate.as_deref(), &cancel));
            next += 1;
        }
        let Some(result) = tasks.join_next().await else {
            break;
        };
        let (index, damaged, bytes) = match result {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                cancel.cancel();
                return Err(error);
            }
            Err(_) => {
                cancel.cancel();
                return Err(VerifyError::Worker);
            }
        };
        bytes_hashed = bytes_hashed.checked_add(bytes).ok_or(VerifyError::Worker)?;
        if damaged {
            damaged_files.push(index);
        }
    }
    damaged_files.sort_unstable();
    Ok(VerifyReport {
        damaged_files,
        bytes_hashed,
    })
}

fn verify_file(
    safe: &SafeRoot,
    index: u32,
    expected: &vgames_core::manifest::File,
    gate: Option<&RateGate>,
    cancel: &CancellationToken,
) -> Result<(u32, bool, u64), VerifyError> {
    if cancel.is_cancelled() {
        return Err(VerifyError::Cancelled);
    }
    let path = safe.path_of(&expected.path);
    // Opened through the root's handle, never by path (INS-07).
    let Some(mut file) = safe.open_existing(&expected.path, Access::Read)? else {
        return Ok((index, true, 0));
    };
    let before = file.metadata().map_err(|source| VerifyError::Io {
        path: path.clone(),
        source,
    })?;
    if !before.is_file() {
        return Err(VerifyError::UnsafePath(SafePathError::NotAFile(path)));
    }
    if before.len() != expected.size || wrong_executable(&before, expected.executable) {
        return Ok((index, true, 0));
    }
    let mut buffer = [0u8; READ_SIZE];
    let mut hasher = blake3::Hasher::new();
    let mut bytes_hashed = 0u64;
    while bytes_hashed < expected.size {
        if cancel.is_cancelled() {
            return Err(VerifyError::Cancelled);
        }
        let to_read = (expected.size - bytes_hashed).min(READ_SIZE as u64) as usize;
        if let Some(gate) = gate {
            gate.wait(to_read, cancel)?;
        }
        let slice = buffer.get_mut(..to_read).ok_or(VerifyError::Worker)?;
        let count = file.read(slice).map_err(|source| VerifyError::Io {
            path: path.clone(),
            source,
        })?;
        if count == 0 {
            return Ok((index, true, bytes_hashed));
        }
        hasher.update(buffer.get(..count).ok_or(VerifyError::Worker)?);
        bytes_hashed += count as u64;
    }
    let after = file
        .metadata()
        .map_err(|source| VerifyError::Io { path, source })?;
    let changed_during_scan =
        after.len() != before.len() || after.modified().ok() != before.modified().ok();
    let digest = Digest::from_bytes(*hasher.finalize().as_bytes());
    Ok((
        index,
        changed_during_scan || digest != expected.blake3,
        bytes_hashed,
    ))
}

#[cfg(unix)]
fn wrong_executable(meta: &fs::Metadata, expected: bool) -> bool {
    use std::os::unix::fs::PermissionsExt;
    (meta.permissions().mode() & 0o111 != 0) != expected
}

#[cfg(not(unix))]
fn wrong_executable(_meta: &fs::Metadata, _expected: bool) -> bool {
    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::testkit::package::{FileSpec, TestPackage};
    use vgames_pack::Compression;

    #[tokio::test]
    async fn reports_only_damaged_or_missing_files_without_mutating_the_tree() {
        let files = [
            FileSpec::random("a.bin", 1024, 1),
            FileSpec::random("nested/b.bin", 2048, 2),
            FileSpec::random("nested/c.bin", 4096, 3),
        ];
        let package = TestPackage::build(&files, &[], Compression::None);
        let root = package.source.path();
        fs::write(root.join("nested/b.bin"), vec![9; 2048]).unwrap();
        fs::remove_file(root.join("nested/c.bin")).unwrap();
        let report = verify_install(
            root,
            Arc::new(package.release()),
            VerifyOptions {
                workers: 3,
                bytes_per_second: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(report.damaged_files, [1, 2]);
        assert_eq!(report.bytes_hashed, 1024 + 2048);
        assert!(!root.join("nested/c.bin").exists());
    }

    #[tokio::test]
    async fn cancellation_stops_before_file_reads() {
        let package = TestPackage::build(
            &[FileSpec::random("large.bin", 1024 * 1024, 4)],
            &[],
            Compression::None,
        );
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = verify_install(
            package.source.path(),
            Arc::new(package.release()),
            VerifyOptions::default(),
            cancel,
        )
        .await;
        assert!(matches!(result, Err(VerifyError::Cancelled)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn linked_parent_is_rejected() {
        let package = TestPackage::build(
            &[FileSpec::random("nested/a.bin", 10, 5)],
            &[],
            Compression::None,
        );
        let root = package.source.path();
        fs::rename(root.join("nested"), root.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink("elsewhere", root.join("nested")).unwrap();
        let result = verify_install(
            root,
            Arc::new(package.release()),
            VerifyOptions::default(),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            result,
            Err(VerifyError::UnsafePath(SafePathError::Link(_)))
        ));
    }
}
