//! Fresh installs (02-package-format §7 and §10), on top of the download
//! engine:
//!
//! 1. [`fetch_release`]: manifest bytes (size + BLAKE3) → `verify_manifest`
//!    (01-security §3.4). Nothing is created on disk before this succeeds.
//! 2. [`install`]: file-size and free-space checks → `.vgames/` with the exact
//!    signed manifest, its signature and `install.json {installing}` →
//!    preallocation of every file → download ([`crate::download::run`]) →
//!    finalize (fsync, exec bits, empty folders, `install.json {installed}`
//!    written last and atomically).
//!
//! An interrupted install resumes from `.vgames/journal.bin` by calling
//! [`install`] again. [`remove_install`] deletes what a manifest lists
//! (cancelled installs, uninstall) without following links.

use std::collections::{BTreeSet, HashSet};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vgames_core::manifest::Manifest;
use vgames_core::trust::TrustState;
use vgames_core::verify::{
    ExpectedRelease, VerifiedManifest, VerifyError, VerifyMode, verify_manifest,
};
use vgames_core::{Digest, Envelope};

use crate::download::journal::{Bitset, Journal, JournalKey};
use crate::download::manifest::{ManifestFetchError, ManifestLink, fetch_manifest};
use crate::download::table::{ChunkTable, TableError};
use crate::download::{
    self, DownloadControl, DownloadError, DownloadOptions, DownloadSpec, PackUrlSource,
    PauseReason, Phase, RunOutcome, RunStats,
};
use crate::fsutil::{self, SafePathError, SafeRoot};
use crate::sys;

pub const INSTALL_FORMAT: &str = "vgames.install/1";
/// Launcher metadata inside an install (a reserved manifest path).
pub const META_DIR: &str = ".vgames";
pub const RECORD_FILE: &str = "install.json";
pub const MANIFEST_FILE: &str = "manifest.json";
pub const SIGNATURE_FILE: &str = "manifest.sig";
pub const JOURNAL_FILE: &str = "journal.bin";
/// Head room kept free on top of the package size (02 §7.2).
pub const SPACE_MARGIN: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallState {
    Installing,
    Installed,
    Updating,
    Repairing,
}

/// `.vgames/install.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRecord {
    pub format: String,
    pub server_id: Uuid,
    pub package_id: Uuid,
    pub version_id: Uuid,
    pub sequence: u64,
    pub platform: String,
    pub state: InstallState,
    /// BLAKE3 of `manifest.json` (the signed bytes).
    pub manifest_blake3: String,
    /// RFC 3339, set when the install completed.
    #[serde(default)]
    pub installed_at: Option<String>,
}

impl InstallRecord {
    pub fn for_manifest(verified: &VerifiedManifest, state: InstallState) -> Self {
        let m = &verified.manifest;
        Self {
            format: INSTALL_FORMAT.to_owned(),
            server_id: m.server_id,
            package_id: m.package_id,
            version_id: m.version_id,
            sequence: m.sequence,
            platform: m.platform.as_str().to_owned(),
            state,
            manifest_blake3: verified.digest.to_hex(),
            installed_at: None,
        }
    }
}

/// A manifest whose signature and contents passed `verify_manifest`, with
/// the exact bytes to store.
#[derive(Debug, Clone)]
pub struct Release {
    pub manifest_bytes: Vec<u8>,
    pub envelope: Envelope,
    pub verified: VerifiedManifest,
}

impl Release {
    pub fn manifest(&self) -> &Manifest {
        &self.verified.manifest
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Manifest(#[from] ManifestFetchError),
    #[error("the package failed verification: {0}")]
    Verify(#[from] VerifyError),
    #[error(transparent)]
    Layout(#[from] TableError),
    #[error("not enough disk space: {required} bytes needed, {available} available")]
    NotEnoughSpace { required: u64, available: u64 },
    #[error("{path} is {size} bytes, more than this drive allows per file ({limit} bytes)")]
    FileTooLarge { path: String, size: u64, limit: u64 },
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    UnsafePath(#[from] SafePathError),
    #[error("cannot {op} {path}: {source}")]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Download(#[from] DownloadError),
    #[error("cancelled")]
    Cancelled,
    #[error("internal error: {0}")]
    Internal(String),
}

impl InstallError {
    pub(crate) fn io(op: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            op,
            path: path.to_owned(),
            source,
        }
    }

    /// Integrity failures are never retried automatically.
    pub fn is_integrity(&self) -> bool {
        matches!(
            self,
            Self::Verify(_)
                | Self::Layout(_)
                | Self::Manifest(ManifestFetchError::Hash | ManifestFetchError::Size)
                | Self::Download(DownloadError::Integrity { .. })
        )
    }
}

/// Verifies manifest bytes against the trust state (01-security §3.4).
pub fn verify_release(
    trust: &TrustState,
    envelope: Envelope,
    manifest_bytes: Vec<u8>,
    expected: &ExpectedRelease,
    installed_sequence: Option<u64>,
    mode: VerifyMode,
) -> Result<Release, VerifyError> {
    let verified = verify_manifest(
        trust,
        &envelope,
        &manifest_bytes,
        expected,
        installed_sequence,
        mode,
    )?;
    Ok(Release {
        manifest_bytes,
        envelope,
        verified,
    })
}

/// Downloads the manifest of a release descriptor and verifies it.
#[allow(clippy::too_many_arguments)]
pub async fn fetch_release(
    client: &reqwest::Client,
    link: &ManifestLink,
    envelope: Envelope,
    trust: &TrustState,
    expected: &ExpectedRelease,
    installed_sequence: Option<u64>,
    mode: VerifyMode,
    stall: Duration,
) -> Result<Release, InstallError> {
    // The descriptor's hash and the signed one must agree before we download.
    if link.blake3 != envelope.payload_blake3 {
        return Err(ManifestFetchError::Hash.into());
    }
    let bytes = fetch_manifest(client, link, stall).await?;
    Ok(verify_release(
        trust,
        envelope,
        bytes,
        expected,
        installed_sequence,
        mode,
    )?)
}

/// Reads and verifies the manifest stored in an install (resume, pre-launch).
pub fn load_local_release(
    root: &Path,
    trust: &TrustState,
    expected: &ExpectedRelease,
    mode: VerifyMode,
) -> Result<Release, InstallError> {
    let meta = root.join(META_DIR);
    let manifest_path = meta.join(MANIFEST_FILE);
    let signature_path = meta.join(SIGNATURE_FILE);
    let bytes =
        fs::read(&manifest_path).map_err(|e| InstallError::io("read", &manifest_path, e))?;
    let sig =
        fs::read(&signature_path).map_err(|e| InstallError::io("read", &signature_path, e))?;
    let envelope = Envelope::parse(&sig)
        .map_err(|e| InstallError::Conflict(format!("the stored signature is unreadable: {e}")))?;
    Ok(verify_release(
        trust, envelope, bytes, expected, None, mode,
    )?)
}

/// Reads `.vgames/install.json`, if present.
pub fn read_record(root: &Path) -> Result<Option<InstallRecord>, InstallError> {
    const MAX_RECORD_BYTES: u64 = 64 * 1024;
    let path = root.join(META_DIR).join(RECORD_FILE);
    if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file() || m.len() > MAX_RECORD_BYTES) {
        return Err(InstallError::Conflict(format!(
            "{} is not a valid install record",
            path.display()
        )));
    }
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| InstallError::Conflict(format!("{} is unreadable: {e}", path.display()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(InstallError::io("read", &path, e)),
    }
}

/// Writes `.vgames/install.json` atomically.
pub fn write_record(root: &Path, record: &InstallRecord) -> Result<(), InstallError> {
    let path = root.join(META_DIR).join(RECORD_FILE);
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|e| InstallError::Internal(format!("install record: {e}")))?;
    fsutil::atomic_write(&path, &bytes).map_err(|e| InstallError::io("write", &path, e))
}

/// How [`install`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed(InstallRecord),
    /// Resumable: call [`install`] again (after `control.resume()`).
    Paused(PauseReason),
    /// The partial install is kept; delete it with [`remove_install`] if the
    /// user does not want to keep it (02 §7.10).
    Cancelled,
    /// Every file of `DownloadOptions::priority_files` is complete, each chunk
    /// verified against the signed manifest; nothing else was requested yet.
    /// `paths` are their locations in the install, in manifest order. Call
    /// [`install`] again without priority files to fetch the rest.
    PriorityFilesReady {
        paths: Vec<PathBuf>,
    },
}

#[derive(Debug, Clone)]
pub struct InstallReport {
    pub outcome: InstallOutcome,
    pub stats: RunStats,
}

/// Space an install needs: `total − reusable + 64 MiB` (02 §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceCheck {
    pub required: u64,
    pub available: u64,
}

/// Installs (or resumes installing) `release` into `root`.
pub async fn install<A: PackUrlSource>(
    root: &Path,
    release: Arc<Release>,
    api: Arc<A>,
    options: &DownloadOptions,
    control: &DownloadControl,
) -> Result<InstallReport, InstallError> {
    control.set_phase(Phase::Allocating);
    let prepared = {
        let root = root.to_owned();
        let release = Arc::clone(&release);
        let control = control.clone();
        tokio::task::spawn_blocking(move || prepare(&root, &release, &control))
            .await
            .map_err(|e| InstallError::Internal(format!("prepare: {e}")))??
    };
    let Prepared {
        safe,
        table,
        targets,
        journal,
    } = prepared;
    let priority = priority_chunks(
        release.manifest(),
        &table,
        &targets,
        &options.priority_files,
    );
    let spec = DownloadSpec {
        table,
        targets: Arc::clone(&targets),
        journal,
        wanted: priority.as_ref().map(|(wanted, _)| wanted.clone()),
    };
    let report = download::run(spec, api, options, control).await?;
    if let Some((_, paths)) = priority {
        let outcome = match report.outcome {
            RunOutcome::Completed => InstallOutcome::PriorityFilesReady { paths },
            RunOutcome::Paused(reason) => InstallOutcome::Paused(reason),
            RunOutcome::Cancelled => InstallOutcome::Cancelled,
        };
        return Ok(InstallReport {
            outcome,
            stats: report.stats,
        });
    }
    let outcome = match report.outcome {
        RunOutcome::Completed => {
            control.set_phase(Phase::Finalizing);
            let release = Arc::clone(&release);
            let record = tokio::task::spawn_blocking(move || finalize(&safe, &release, &targets))
                .await
                .map_err(|e| InstallError::Internal(format!("finalize: {e}")))??;
            control.set_phase(Phase::Done);
            InstallOutcome::Installed(record)
        }
        RunOutcome::Paused(reason) => InstallOutcome::Paused(reason),
        RunOutcome::Cancelled => InstallOutcome::Cancelled,
    };
    Ok(InstallReport {
        outcome,
        stats: report.stats,
    })
}

/// The chunks and target paths of the priority files, or `None` without any.
fn priority_chunks(
    manifest: &Manifest,
    table: &ChunkTable,
    targets: &[Option<PathBuf>],
    wanted_paths: &[String],
) -> Option<(Bitset, Vec<PathBuf>)> {
    if wanted_paths.is_empty() {
        return None;
    }
    let wanted_paths: HashSet<&str> = wanted_paths.iter().map(String::as_str).collect();
    let mut wanted = Bitset::new(table.len());
    let mut paths = Vec::new();
    for (index, file) in manifest.files.iter().enumerate() {
        if !wanted_paths.contains(file.path.as_str()) {
            continue;
        }
        if let Some(first) = file.chunk {
            let count = if file.size >= manifest.chunk_size {
                file.size.div_ceil(manifest.chunk_size)
            } else {
                1
            };
            for k in 0..count {
                if let Some(chunk) = u32::try_from(k).ok().and_then(|k| first.checked_add(k)) {
                    wanted.set(chunk);
                }
            }
        }
        if let Some(Some(path)) = targets.get(index) {
            paths.push(path.clone());
        }
    }
    Some((wanted, paths))
}

struct Prepared {
    safe: SafeRoot,
    table: Arc<ChunkTable>,
    targets: Arc<Vec<Option<PathBuf>>>,
    journal: Journal,
}

/// The closest existing ancestor of `path` (for filesystem queries before
/// the install folder exists).
fn existing_ancestor(path: &Path) -> PathBuf {
    let mut probe = path.to_owned();
    while !probe.exists() {
        match probe.parent() {
            Some(parent) if parent != probe => probe = parent.to_owned(),
            _ => break,
        }
    }
    probe
}

/// Checks the per-file size limit and free space for installing `manifest`
/// into `root` (files already present at their final size count as reusable).
pub fn check_space(root: &Path, manifest: &Manifest) -> Result<SpaceCheck, InstallError> {
    let probe = existing_ancestor(root);
    if let Some(limit) =
        sys::max_file_size(&probe).map_err(|e| InstallError::io("inspect", &probe, e))?
        && let Some(big) = manifest.files.iter().find(|f| f.size > limit)
    {
        return Err(InstallError::FileTooLarge {
            path: big.path.clone(),
            size: big.size,
            limit,
        });
    }
    let mut reusable = 0u64;
    if root.exists() {
        for file in &manifest.files {
            let path = file.path.split('/').fold(root.to_owned(), |p, c| p.join(c));
            if let Ok(meta) = fs::symlink_metadata(&path)
                && meta.is_file()
            {
                reusable += meta.len().min(file.size);
            }
        }
    }
    let total: u64 = manifest.files.iter().map(|f| f.size).sum();
    let required = total.saturating_sub(reusable) + SPACE_MARGIN;
    let available =
        sys::available_space(&probe).map_err(|e| InstallError::io("inspect", &probe, e))?;
    if required > available {
        return Err(InstallError::NotEnoughSpace {
            required,
            available,
        });
    }
    Ok(SpaceCheck {
        required,
        available,
    })
}

fn prepare(
    root: &Path,
    release: &Release,
    control: &DownloadControl,
) -> Result<Prepared, InstallError> {
    let manifest = release.manifest();
    match read_record(root)? {
        Some(record) => {
            if record.server_id != manifest.server_id || record.package_id != manifest.package_id {
                return Err(InstallError::Conflict(format!(
                    "{} holds another package",
                    root.display()
                )));
            }
            if record.version_id != manifest.version_id {
                return Err(InstallError::Conflict(format!(
                    "{} holds another version of this package; update it instead",
                    root.display()
                )));
            }
        }
        None => {
            if let Ok(entries) = fs::read_dir(root)
                && entries
                    .flatten()
                    .any(|e| e.file_name() != std::ffi::OsStr::new(META_DIR))
            {
                return Err(InstallError::Conflict(format!(
                    "{} is not empty",
                    root.display()
                )));
            }
        }
    }
    let space = check_space(root, manifest)?;

    let safe = SafeRoot::create(root)?;
    let meta = safe.ensure_dir(META_DIR)?;
    write_if_changed(&meta.join(MANIFEST_FILE), &release.manifest_bytes)?;
    write_if_changed(&meta.join(SIGNATURE_FILE), &release.envelope.to_bytes())?;
    write_record(
        safe.root(),
        &InstallRecord::for_manifest(&release.verified, InstallState::Installing),
    )?;

    let table = ChunkTable::new(manifest)?;
    let mut targets = Vec::with_capacity(manifest.files.len());
    let journal_path = meta.join(JOURNAL_FILE);
    let mut journal_invalidated = false;
    for (i, file) in manifest.files.iter().enumerate() {
        if i % 256 == 0 && control.is_cancelled() {
            return Err(InstallError::Cancelled);
        }
        let path = safe.file_target(&file.path)?;
        if !journal_invalidated && !fs::symlink_metadata(&path).is_ok_and(|m| m.len() == file.size)
        {
            // A journal bit describes bytes in the previous file, not a newly
            // created or resized replacement. Invalidate before allocation,
            // so a crash here cannot leave zeros recorded as completed data.
            match fs::remove_file(&journal_path) {
                Ok(()) => {
                    fsutil::sync_dir(&meta).map_err(|e| InstallError::io("flush", &meta, e))?
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(InstallError::io("delete", &journal_path, e)),
            }
            journal_invalidated = true;
        }
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let handle = options
            .open(&path)
            .map_err(|e| InstallError::io("create", &path, e))?;
        if let Err(error) = sys::preallocate(&handle, file.size) {
            if sys::is_disk_full(&error) {
                return Err(InstallError::NotEnoughSpace {
                    required: space.required,
                    available: sys::available_space(safe.root()).unwrap_or(0),
                });
            }
            return Err(InstallError::io("allocate", &path, error));
        }
        targets.push(Some(path));
    }
    let journal = Journal::load_or_new(
        &journal_path,
        JournalKey {
            version_id: manifest.version_id,
            manifest_blake3: *release.verified.digest.as_bytes(),
            chunk_count: table.len(),
        },
    );
    Ok(Prepared {
        safe,
        table: Arc::new(table),
        targets: Arc::new(targets),
        journal,
    })
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<(), InstallError> {
    if fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    fsutil::atomic_write(path, bytes).map_err(|e| InstallError::io("write", path, e))
}

fn finalize(
    safe: &SafeRoot,
    release: &Release,
    targets: &[Option<PathBuf>],
) -> Result<InstallRecord, InstallError> {
    let manifest = release.manifest();
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for (file, target) in manifest.files.iter().zip(targets) {
        let Some(path) = target else { continue };
        let handle = fsutil::open_for_write(path).map_err(|e| InstallError::io("open", path, e))?;
        handle
            .sync_all()
            .map_err(|e| InstallError::io("flush", path, e))?;
        set_executable(&handle, file.executable)
            .map_err(|e| InstallError::io("set permissions of", path, e))?;
        if let Some(parent) = path.parent() {
            dirs.insert(parent.to_owned());
        }
    }
    for dir in &manifest.directories {
        dirs.insert(safe.ensure_dir(dir)?);
    }
    for dir in &dirs {
        fsutil::sync_dir(dir).map_err(|e| InstallError::io("flush", dir, e))?;
    }
    let mut record = InstallRecord::for_manifest(&release.verified, InstallState::Installed);
    record.installed_at = Some(now_rfc3339());
    write_record(safe.root(), &record)?;
    let journal = safe.root().join(META_DIR).join(JOURNAL_FILE);
    match fs::remove_file(&journal) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(InstallError::io("delete", &journal, e)),
    }
    Ok(record)
}

/// Sets or clears the executable bits (Unix; no-op elsewhere).
pub fn set_executable(file: &fs::File, executable: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = file.metadata()?.permissions();
        let mode = permissions.mode();
        let wanted = if executable {
            mode | ((mode & 0o444) >> 2)
        } else {
            mode & !0o111
        };
        if wanted != mode {
            permissions.set_mode(wanted);
            file.set_permissions(permissions)?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (file, executable);
    }
    Ok(())
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// What [`remove_install`] could not delete (user files, links).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Leftovers {
    pub paths: Vec<PathBuf>,
}

/// Read-only preview for an uninstall confirmation. `paths` are relative to
/// the install root; symlinks and non-regular entries are reported, not opened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UninstallPreview {
    pub paths: Vec<PathBuf>,
    /// More than 10,000 entries remain; the UI should show a count limit.
    pub truncated: bool,
}

/// Lists files and empty folders the signed manifest does not own, before
/// [`remove_install`] changes anything. The default uninstall keeps them.
pub fn preview_uninstall(
    root: &Path,
    manifest: &Manifest,
) -> Result<UninstallPreview, InstallError> {
    let safe = SafeRoot::open(root)?;
    let mut known_files = HashSet::new();
    let mut known_dirs = HashSet::new();
    for file in &manifest.files {
        let path = safe.path_of(&file.path);
        add_parents(&path, safe.root(), &mut known_dirs);
        known_files.insert(path);
    }
    for dir in &manifest.directories {
        let path = safe.path_of(dir);
        add_parents(&path, safe.root(), &mut known_dirs);
        known_dirs.insert(path);
    }
    let meta = safe.path_of(META_DIR);
    let mut preview = UninstallPreview::default();
    let mut pending = vec![safe.root().to_owned()];
    while let Some(dir) = pending.pop() {
        let entries = fs::read_dir(&dir).map_err(|error| InstallError::io("read", &dir, error))?;
        let mut empty = true;
        for entry in entries {
            let entry = entry.map_err(|error| InstallError::io("read", &dir, error))?;
            empty = false;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| InstallError::io("inspect", &path, error))?;
            if path == meta && metadata.is_dir() && !metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                pending.push(path);
            } else if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || !known_files.contains(&path)
            {
                preview
                    .paths
                    .push(path.strip_prefix(safe.root()).unwrap_or(&path).to_owned());
            }
            if preview.paths.len() >= 10_000 {
                preview.truncated = true;
                preview.paths.sort();
                return Ok(preview);
            }
        }
        if empty && dir != safe.root() && !known_dirs.contains(&dir) {
            preview
                .paths
                .push(dir.strip_prefix(safe.root()).unwrap_or(&dir).to_owned());
        }
    }
    preview.paths.sort();
    Ok(preview)
}

fn add_parents(path: &Path, root: &Path, known_dirs: &mut HashSet<PathBuf>) {
    let mut current = path.parent();
    while let Some(parent) = current {
        if parent == root {
            break;
        }
        known_dirs.insert(parent.to_owned());
        current = parent.parent();
    }
}

/// Deletes every file the manifest lists, the launcher metadata and the
/// folders that became empty, never following links (02 §9). Anything else
/// (saves, mods, configs, links) is left in place and returned, so the caller
/// can ask the user; `root` itself is removed only when empty.
pub fn remove_install(root: &Path, manifest: &Manifest) -> Result<Leftovers, InstallError> {
    let meta = fs::symlink_metadata(root).map_err(|e| InstallError::io("open", root, e))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(SafePathError::Link(root.to_owned()).into());
    }
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for file in &manifest.files {
        let mut path = root.to_owned();
        let parts: Vec<&str> = file.path.split('/').collect();
        let mut through_link = false;
        for (i, part) in parts.iter().enumerate() {
            path.push(part);
            if i + 1 < parts.len() {
                match fs::symlink_metadata(&path) {
                    Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {
                        dirs.insert(path.clone());
                    }
                    _ => {
                        through_link = true;
                        break;
                    }
                }
            }
        }
        if through_link {
            continue;
        }
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_file() => {
                fs::remove_file(&path).map_err(|e| InstallError::io("delete", &path, e))?
            }
            _ => {}
        }
    }
    for dir in &manifest.directories {
        let mut path = root.to_owned();
        let mut plain = Vec::new();
        for part in dir.split('/') {
            path.push(part);
            match fs::symlink_metadata(&path) {
                Ok(m) if m.is_dir() && !m.file_type().is_symlink() => plain.push(path.clone()),
                _ => break,
            }
        }
        dirs.extend(plain);
    }
    let meta_dir = root.join(META_DIR);
    if fs::symlink_metadata(&meta_dir).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink()) {
        for entry in fs::read_dir(&meta_dir)
            .map_err(|e| InstallError::io("read", &meta_dir, e))?
            .flatten()
        {
            let path = entry.path();
            match fs::symlink_metadata(&path) {
                Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {
                    remove_tree_no_follow(&path)?;
                }
                Ok(_) => {
                    fs::remove_file(&path).map_err(|e| InstallError::io("delete", &path, e))?
                }
                Err(_) => {}
            }
        }
        dirs.insert(meta_dir);
    }
    // Deepest first, so parents empty out.
    let mut ordered: Vec<PathBuf> = dirs.into_iter().collect();
    ordered.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for dir in ordered {
        let _ = fs::remove_dir(&dir);
    }
    let _ = fs::remove_dir(root);
    let mut leftovers = Leftovers::default();
    if root.exists() {
        collect_leftovers(root, &mut leftovers.paths);
    }
    Ok(leftovers)
}

/// Deletes a launcher-owned tree (staging) without following links.
pub fn remove_tree_no_follow(dir: &Path) -> Result<(), InstallError> {
    for entry in fs::read_dir(dir)
        .map_err(|e| InstallError::io("read", dir, e))?
        .flatten()
    {
        let path = entry.path();
        let meta =
            fs::symlink_metadata(&path).map_err(|e| InstallError::io("inspect", &path, e))?;
        if meta.is_dir() && !meta.file_type().is_symlink() {
            remove_tree_no_follow(&path)?;
        } else {
            // Removes the link itself, never its target.
            remove_entry(&path, &meta)?;
        }
    }
    fs::remove_dir(dir).map_err(|e| InstallError::io("delete", dir, e))
}

fn remove_entry(path: &Path, meta: &fs::Metadata) -> Result<(), InstallError> {
    #[cfg(windows)]
    if meta.file_type().is_symlink() && meta.is_dir() {
        return fs::remove_dir(path).map_err(|e| InstallError::io("delete", path, e));
    }
    let _ = meta;
    fs::remove_file(path).map_err(|e| InstallError::io("delete", path, e))
}

fn collect_leftovers(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => collect_leftovers(&path, out),
            _ => out.push(path),
        }
        if out.len() >= 10_000 {
            return;
        }
    }
}

/// The folder name for a new install: `slug`, or `slug-2`, `slug-3`… when
/// taken (02 §10).
pub fn free_dir_name(library: &Path, slug: &str) -> String {
    if !library.join(slug).exists() {
        return slug.to_owned();
    }
    (2u32..)
        .map(|n| format!("{slug}-{n}"))
        .find(|name| !library.join(name).exists())
        .unwrap_or_else(|| format!("{slug}-{}", Uuid::now_v7()))
}

/// BLAKE3 of the stored manifest bytes, for comparing with `install.json`.
pub fn manifest_digest(root: &Path) -> Result<Digest, InstallError> {
    let path = root.join(META_DIR).join(MANIFEST_FILE);
    fs::read(&path)
        .map(|b| Digest::of(&b))
        .map_err(|e| InstallError::io("read", &path, e))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod uninstall_preview_tests {
    use super::*;
    use crate::testkit::package::{FileSpec, TestPackage, write_tree};
    use vgames_pack::Compression;

    #[test]
    fn previews_leftovers_before_uninstall_and_keeps_them_by_default() {
        let file = FileSpec::random("bin/game", 16, 4);
        let package = TestPackage::build(
            std::slice::from_ref(&file),
            &["known-empty"],
            Compression::None,
        );
        let root = tempfile::tempdir().unwrap();
        write_tree(root.path(), &[file], &["known-empty", "user-empty"]);
        fs::write(root.path().join("bin/config.ini"), b"player settings").unwrap();
        let preview = preview_uninstall(root.path(), package.release().manifest()).unwrap();
        assert_eq!(
            preview.paths,
            vec![PathBuf::from("bin/config.ini"), PathBuf::from("user-empty")]
        );
        assert!(!preview.truncated);
        let leftovers = remove_install(root.path(), package.release().manifest()).unwrap();
        assert!(
            leftovers
                .paths
                .iter()
                .any(|path| path.ends_with("bin/config.ini"))
        );
        assert_eq!(
            fs::read(root.path().join("bin/config.ini")).unwrap(),
            b"player settings"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_at_a_signed_file_path_is_reported_without_following_it() {
        use std::os::unix::fs::symlink;
        let file = FileSpec::random("game", 8, 9);
        let package = TestPackage::build(std::slice::from_ref(&file), &[], Compression::None);
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write_tree(root.path(), &[file], &[]);
        fs::remove_file(root.path().join("game")).unwrap();
        fs::write(outside.path().join("secret"), b"untouched").unwrap();
        symlink(outside.path().join("secret"), root.path().join("game")).unwrap();
        let preview = preview_uninstall(root.path(), package.release().manifest()).unwrap();
        assert_eq!(preview.paths, vec![PathBuf::from("game")]);
        assert_eq!(
            fs::read(outside.path().join("secret")).unwrap(),
            b"untouched"
        );
    }
}
