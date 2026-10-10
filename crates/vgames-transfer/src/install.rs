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
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vgames_core::manifest::MAX_MANIFEST_BYTES;
use vgames_core::manifest::Manifest;
use vgames_core::sign::MAX_ENVELOPE_BYTES;
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
use crate::fsutil::{Access, SafePathError, SafeRoot, Target};
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
    let safe = SafeRoot::open_resolved(root).map_err(|e| path_error("read", e))?;
    let bytes = read_meta(&safe, MANIFEST_FILE, MAX_MANIFEST_BYTES as u64)?;
    let sig = read_meta(&safe, SIGNATURE_FILE, MAX_ENVELOPE_BYTES as u64)?;
    let envelope = Envelope::parse(&sig)
        .map_err(|e| InstallError::Conflict(format!("the stored signature is unreadable: {e}")))?;
    Ok(verify_release(
        trust, envelope, bytes, expected, None, mode,
    )?)
}

/// Verifies the stored manifest under a new `envelope` (a re-signed release) and, when it
/// passes, stores that signature in place of the old one.
pub fn adopt_signature(
    root: &Path,
    trust: &TrustState,
    envelope: Envelope,
    expected: &ExpectedRelease,
) -> Result<Release, InstallError> {
    let safe = SafeRoot::open_resolved(root).map_err(|e| path_error("read", e))?;
    let bytes = read_meta(&safe, MANIFEST_FILE, MAX_MANIFEST_BYTES as u64)?;
    let release = verify_release(trust, envelope, bytes, expected, None, VerifyMode::Launch)?;
    safe.atomic_write(&meta_rel(SIGNATURE_FILE), &release.envelope.to_bytes())
        .map_err(|e| path_error("write", e))?;
    Ok(release)
}

/// Reads `.vgames/install.json`, if present.
pub fn read_record(root: &Path) -> Result<Option<InstallRecord>, InstallError> {
    match SafeRoot::open_resolved(root) {
        Ok(safe) => read_record_in(&safe),
        Err(SafePathError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(e) => Err(path_error("read", e)),
    }
}

/// [`read_record`] through an open root.
pub(crate) fn read_record_in(safe: &SafeRoot) -> Result<Option<InstallRecord>, InstallError> {
    const MAX_RECORD_BYTES: u64 = 64 * 1024;
    let rel = meta_rel(RECORD_FILE);
    let path = safe.path_of(&rel);
    let bytes = match safe.read(&rel, MAX_RECORD_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(None),
        Err(SafePathError::Io { source, .. }) if source.kind() != io::ErrorKind::InvalidData => {
            return Err(InstallError::io("read", &path, source));
        }
        Err(_) => {
            return Err(InstallError::Conflict(format!(
                "{} is not a valid install record",
                path.display()
            )));
        }
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| InstallError::Conflict(format!("{} is unreadable: {e}", path.display())))
}

/// Writes `.vgames/install.json` atomically.
pub fn write_record(root: &Path, record: &InstallRecord) -> Result<(), InstallError> {
    let safe = SafeRoot::open(root).map_err(|e| path_error("write", e))?;
    write_record_in(&safe, record)
}

/// [`write_record`] through an open root.
pub(crate) fn write_record_in(safe: &SafeRoot, record: &InstallRecord) -> Result<(), InstallError> {
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|e| InstallError::Internal(format!("install record: {e}")))?;
    safe.atomic_write(&meta_rel(RECORD_FILE), &bytes)
        .map_err(|e| path_error("write", e))
}

/// `name` inside the launcher's metadata folder, as a root-relative path.
pub(crate) fn meta_rel(name: &str) -> String {
    format!("{META_DIR}/{name}")
}

/// A refused path stays [`InstallError::UnsafePath`]; an OS error becomes [`InstallError::Io`] so
/// callers can tell a missing install (`NotFound`) apart.
pub(crate) fn path_error(op: &'static str, error: SafePathError) -> InstallError {
    match error {
        SafePathError::Io { path, source } => InstallError::Io { op, path, source },
        other => other.into(),
    }
}

/// Reads a metadata file that must exist (a missing one is `NotFound`).
fn read_meta(safe: &SafeRoot, name: &str, max: u64) -> Result<Vec<u8>, InstallError> {
    let rel = meta_rel(name);
    safe.read(&rel, max)
        .map_err(|e| path_error("read", e))?
        .ok_or_else(|| {
            InstallError::io(
                "read",
                &safe.path_of(&rel),
                io::Error::from(io::ErrorKind::NotFound),
            )
        })
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
    targets: &[Option<Target>],
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
        if let Some(Some(target)) = targets.get(index) {
            paths.push(target.display_path());
        }
    }
    Some((wanted, paths))
}

struct Prepared {
    safe: Arc<SafeRoot>,
    table: Arc<ChunkTable>,
    targets: Arc<Vec<Option<Target>>>,
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

    let safe = Arc::new(SafeRoot::create(root)?);
    safe.ensure_dir(META_DIR)?;
    write_if_changed(
        &safe,
        MANIFEST_FILE,
        &release.manifest_bytes,
        MAX_MANIFEST_BYTES as u64,
    )?;
    write_if_changed(
        &safe,
        SIGNATURE_FILE,
        &release.envelope.to_bytes(),
        MAX_ENVELOPE_BYTES as u64,
    )?;
    write_record_in(
        &safe,
        &InstallRecord::for_manifest(&release.verified, InstallState::Installing),
    )?;

    let table = ChunkTable::new(manifest)?;
    let mut targets = Vec::with_capacity(manifest.files.len());
    let journal_rel = meta_rel(JOURNAL_FILE);
    let mut journal_invalidated = false;
    for (i, file) in manifest.files.iter().enumerate() {
        if i % 256 == 0 && control.is_cancelled() {
            return Err(InstallError::Cancelled);
        }
        // Created (with its folders) through the root's handle, never by path (INS-07).
        let path = safe.path_of(&file.path);
        let handle = safe.open_file(&file.path, Access::CreateWrite)?;
        if !journal_invalidated && !handle.metadata().is_ok_and(|m| m.len() == file.size) {
            // A journal bit describes bytes in the previous file, not a newly
            // created or resized replacement. Invalidate before allocation,
            // so a crash here cannot leave zeros recorded as completed data.
            if safe
                .remove_file(&journal_rel)
                .map_err(|e| path_error("delete", e))?
            {
                safe.sync_dir(META_DIR)
                    .map_err(|e| path_error("flush", e))?;
            }
            journal_invalidated = true;
        }
        if let Err(error) = sys::preallocate(&handle, file.size) {
            if sys::is_disk_full(&error) {
                return Err(InstallError::NotEnoughSpace {
                    required: space.required,
                    available: sys::available_space(safe.root()).unwrap_or(0),
                });
            }
            return Err(InstallError::io("allocate", &path, error));
        }
        targets.push(Some(Target::new(Arc::clone(&safe), file.path.clone())));
    }
    let journal = Journal::in_root(
        Arc::clone(&safe),
        &journal_rel,
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

fn write_if_changed(
    safe: &SafeRoot,
    name: &str,
    bytes: &[u8],
    max: u64,
) -> Result<(), InstallError> {
    let rel = meta_rel(name);
    if safe
        .read(&rel, max)
        .is_ok_and(|current| current.as_deref() == Some(bytes))
    {
        return Ok(());
    }
    safe.atomic_write(&rel, bytes)
        .map_err(|e| path_error("write", e))
}

fn finalize(
    safe: &SafeRoot,
    release: &Release,
    targets: &[Option<Target>],
) -> Result<InstallRecord, InstallError> {
    let manifest = release.manifest();
    let mut dirs: BTreeSet<&str> = BTreeSet::new();
    for (file, target) in manifest.files.iter().zip(targets) {
        let Some(target) = target else { continue };
        let path = &target.display_path();
        let handle = target
            .open_write()
            .map_err(|e| InstallError::io("open", path, e))?;
        handle
            .sync_all()
            .map_err(|e| InstallError::io("flush", path, e))?;
        set_executable(&handle, file.executable)
            .map_err(|e| InstallError::io("set permissions of", path, e))?;
        dirs.insert(file.path.rsplit_once('/').map_or("", |(parent, _)| parent));
    }
    for dir in &manifest.directories {
        safe.ensure_dir(dir)?;
        dirs.insert(dir);
    }
    for dir in dirs {
        safe.sync_dir(dir).map_err(|e| path_error("flush", e))?;
    }
    let mut record = InstallRecord::for_manifest(&release.verified, InstallState::Installed);
    record.installed_at = Some(now_rfc3339());
    write_record_in(safe, &record)?;
    safe.remove_file(&meta_rel(JOURNAL_FILE))
        .map_err(|e| path_error("delete", e))?;
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
    let safe = SafeRoot::open(root).map_err(|e| path_error("open", e))?;
    let mut dirs: BTreeSet<&str> = BTreeSet::new();
    for file in &manifest.files {
        // Every folder is opened through the root's handle: a folder swapped for a link is
        // refused, never followed, and what it holds stays a leftover.
        match safe.remove_regular_file(&file.path) {
            Ok(_) => {}
            Err(SafePathError::Io { path, source }) => {
                return Err(InstallError::io("delete", &path, source));
            }
            Err(_) => continue,
        }
        add_rel_parents(&file.path, &mut dirs);
    }
    for dir in &manifest.directories {
        dirs.insert(dir);
        add_rel_parents(dir, &mut dirs);
    }
    match safe.remove_tree(META_DIR) {
        Ok(_) | Err(SafePathError::Link(_) | SafePathError::NotADirectory(_)) => {}
        Err(e) => return Err(path_error("delete", e)),
    }
    // Deepest first, so parents empty out.
    let mut ordered: Vec<&str> = dirs.into_iter().collect();
    ordered.sort_by_key(|d| std::cmp::Reverse(d.split('/').count()));
    for dir in ordered {
        let _ = safe.remove_empty_dir(dir);
    }
    // Windows refuses to delete a folder while a handle on it is open. Leftovers are reported
    // under the path the caller gave, not its canonical form.
    drop(safe);
    let _ = fs::remove_dir(root);
    let mut leftovers = Leftovers::default();
    if root.exists() {
        collect_leftovers(root, &mut leftovers.paths);
    }
    Ok(leftovers)
}

fn add_rel_parents<'a>(rel: &'a str, dirs: &mut BTreeSet<&'a str>) {
    let mut current = rel;
    while let Some((parent, _)) = current.rsplit_once('/') {
        dirs.insert(parent);
        current = parent;
    }
}

/// Deletes a launcher-owned tree (staging) without following links.
pub fn remove_tree_no_follow(dir: &Path) -> Result<(), InstallError> {
    let safe = SafeRoot::open(dir).map_err(|e| path_error("delete", e))?;
    safe.remove_tree("").map_err(|e| path_error("delete", e))?;
    // Windows refuses to delete a folder while a handle on it is open.
    let root = safe.root().to_owned();
    drop(safe);
    fs::remove_dir(&root).map_err(|e| InstallError::io("delete", &root, e))
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
    let safe = SafeRoot::open_resolved(root).map_err(|e| path_error("read", e))?;
    read_meta(&safe, MANIFEST_FILE, MAX_MANIFEST_BYTES as u64).map(|b| Digest::of(&b))
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

    /// A folder swapped for a link to somewhere else: nothing behind the link is deleted.
    #[cfg(unix)]
    #[test]
    fn uninstall_never_deletes_through_a_folder_swapped_for_a_link() {
        use std::os::unix::fs::symlink;
        let file = FileSpec::random("bin/game", 8, 9);
        let package = TestPackage::build(std::slice::from_ref(&file), &[], Compression::None);
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write_tree(root.path(), &[file], &[]);
        write_record(
            root.path(),
            &InstallRecord::for_manifest(&package.release().verified, InstallState::Installed),
        )
        .unwrap();
        fs::write(outside.path().join("game"), b"not the launcher's").unwrap();
        fs::create_dir(outside.path().join(META_DIR)).unwrap();
        fs::write(outside.path().join(META_DIR).join("keep"), b"kept").unwrap();
        fs::remove_dir_all(root.path().join("bin")).unwrap();
        symlink(outside.path(), root.path().join("bin")).unwrap();

        let leftovers = remove_install(root.path(), package.release().manifest()).unwrap();
        assert_eq!(leftovers.paths, vec![root.path().join("bin")]);
        assert!(!root.path().join(META_DIR).exists());
        assert_eq!(
            fs::read(outside.path().join("game")).unwrap(),
            b"not the launcher's"
        );
        assert_eq!(
            fs::read(outside.path().join(META_DIR).join("keep")).unwrap(),
            b"kept"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_launcher_tree_is_deleted_without_following_links_inside() {
        use std::os::unix::fs::symlink;
        let tree = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"kept").unwrap();
        let staging = tree.path().join("staging");
        fs::create_dir_all(staging.join("a/b")).unwrap();
        fs::write(staging.join("a/b/file"), b"x").unwrap();
        symlink(outside.path(), staging.join("a/link")).unwrap();
        remove_tree_no_follow(&staging).unwrap();
        assert!(!staging.exists());
        assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"kept");
    }

    #[cfg(unix)]
    #[test]
    fn an_install_record_that_is_a_link_is_refused() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("install.json"), b"{}").unwrap();
        fs::create_dir(root.path().join(META_DIR)).unwrap();
        symlink(
            outside.path().join("install.json"),
            root.path().join(META_DIR).join(RECORD_FILE),
        )
        .unwrap();
        assert!(matches!(
            read_record(root.path()),
            Err(InstallError::Conflict(_))
        ));
        assert!(read_record(&root.path().join("missing")).unwrap().is_none());
    }
}
