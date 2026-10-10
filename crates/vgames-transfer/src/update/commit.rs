//! Durable, replayable safe-update commit. The caller stages and verifies all
//! changed files before this module writes `commit.json`; after that point a
//! restart must call [`recover_pending`] before exposing the install as playable.

use std::io::{self, Read};
use std::path::Path;

use super::plan::UpdatePlan;
use crate::fsutil::{Access, SafePathError, SafeRoot};
use crate::install::{
    InstallError, InstallRecord, InstallState, MANIFEST_FILE, META_DIR, Release, SIGNATURE_FILE,
    read_record, set_executable, verify_release, write_record,
};
use serde::{Deserialize, Serialize};
use vgames_core::Envelope;
use vgames_core::manifest::parse_and_validate;
use vgames_core::trust::TrustState;
use vgames_core::verify::{ExpectedRelease, VerifyMode};

const COMMIT_FILE: &str = "commit.json";
const STAGING_DIR: &str = ".vgames/staging";
const NEXT_MANIFEST: &str = ".vgames/next-manifest.json";
const NEXT_SIGNATURE: &str = ".vgames/next-manifest.sig";
const OLD_MANIFEST: &str = ".vgames/old-manifest.json";
const OLD_SIGNATURE: &str = ".vgames/old-manifest.sig";
const MAX_COMMIT_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitMarker {
    format: String,
    old_digest: String,
    new_digest: String,
    #[serde(default)]
    damaged_files: Vec<u32>,
    #[serde(default)]
    mode: CommitMode,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommitMode {
    #[default]
    Safe,
    InPlace,
}

fn marker_rel() -> String {
    format!("{META_DIR}/{COMMIT_FILE}")
}

/// Every install file below is read, written, renamed and deleted through the install root's
/// folder handle, never by path (INS-07).
fn read_marker(root: &Path) -> Result<Option<CommitMarker>, InstallError> {
    let safe = SafeRoot::open(root)?;
    let bytes = match safe.read(&marker_rel(), MAX_COMMIT_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(None),
        Err(SafePathError::Io { source, .. }) if source.kind() == io::ErrorKind::InvalidData => {
            return Err(InstallError::Conflict(
                "invalid update commit marker".into(),
            ));
        }
        Err(SafePathError::Link(_) | SafePathError::NotAFile(_)) => {
            return Err(InstallError::Conflict(
                "invalid update commit marker".into(),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let marker: CommitMarker = serde_json::from_slice(&bytes)
        .map_err(|_| InstallError::Conflict("invalid update commit marker".into()))?;
    if marker.format != "vgames.commit/1" {
        return Err(InstallError::Conflict(
            "unsupported update commit marker".into(),
        ));
    }
    Ok(Some(marker))
}

/// Whether startup recovery must finish before another transfer or launch.
pub fn is_pending(root: &Path) -> Result<bool, InstallError> {
    Ok(read_marker(root)?.is_some())
}

fn ensure_record(root: &Path, old: &Release, new: &Release) -> Result<(), InstallError> {
    let record = read_record(root)?
        .ok_or_else(|| InstallError::Conflict("missing install record".into()))?;
    if record.server_id != old.manifest().server_id
        || record.package_id != old.manifest().package_id
        || (record.version_id != old.manifest().version_id
            && record.version_id != new.manifest().version_id)
    {
        return Err(InstallError::Conflict(
            "install record does not match the update".into(),
        ));
    }
    Ok(())
}

/// Checks file `rel` (under `safe`) against the signed manifest entry; `false` when it is missing.
fn verify_file(
    safe: &SafeRoot,
    rel: &str,
    expected: &vgames_core::manifest::File,
) -> Result<bool, InstallError> {
    let path = &safe.path_of(rel);
    let Some(mut handle) = safe.open_existing(rel, Access::Read)? else {
        return Ok(false);
    };
    let meta = handle
        .metadata()
        .map_err(|error| InstallError::io("inspect", path, error))?;
    if meta.len() != expected.size {
        return Err(InstallError::Conflict(format!(
            "{} does not match the signed manifest",
            path.display()
        )));
    }
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 256 * 1024];
    loop {
        let count = handle
            .read(&mut buffer)
            .map_err(|error| InstallError::io("read", path, error))?;
        if count == 0 {
            break;
        }
        hasher.update(buffer.get(..count).unwrap_or_default());
    }
    if hasher.finalize().as_bytes() != expected.blake3.as_bytes() {
        return Err(InstallError::Conflict(format!(
            "{} does not match the signed manifest",
            path.display()
        )));
    }
    Ok(true)
}

/// The installed manifest's bytes, through the root's handle.
fn read_installed_manifest(safe: &SafeRoot) -> Result<Option<Vec<u8>>, InstallError> {
    Ok(safe.read(
        &format!("{META_DIR}/{MANIFEST_FILE}"),
        vgames_core::manifest::MAX_MANIFEST_BYTES as u64,
    )?)
}

fn staged(rel: &str) -> String {
    format!("{STAGING_DIR}/{rel}")
}

/// Marks a fully staged safe update ready to commit, then applies it. `old`
/// and `new` must have passed `verify_manifest`; the plan is recomputed here
/// so callers cannot inject arbitrary deletion paths.
pub fn begin(root: &Path, old: &Release, new: &Release) -> Result<InstallRecord, InstallError> {
    begin_with_damaged(root, old, new, &[])
}

/// Commits a repair or an update that also replaces damaged old files.
pub fn begin_with_damaged(
    root: &Path,
    old: &Release,
    new: &Release,
    damaged_files: &[u32],
) -> Result<InstallRecord, InstallError> {
    if read_marker(root)?.is_some() {
        return replay(root, old, new)?
            .ok_or_else(|| InstallError::Conflict("commit marker vanished".into()));
    }
    let plan = UpdatePlan::new_with_damaged(old, new, true, damaged_files)
        .map_err(|error| InstallError::Conflict(format!("invalid update: {error}")))?;
    ensure_record(root, old, new)?;
    let safe = SafeRoot::open(root)?;
    if read_installed_manifest(&safe)?
        .ok_or_else(|| InstallError::Conflict("missing installed manifest".into()))?
        != old.manifest_bytes
    {
        return Err(InstallError::Conflict(
            "installed manifest changed before commit".into(),
        ));
    }
    for &index in &plan.build_files {
        let file = new
            .manifest()
            .files
            .get(index as usize)
            .ok_or_else(|| InstallError::Conflict("invalid update file index".into()))?;
        let rel = staged(&file.path);
        let path = safe.path_of(&rel);
        if !verify_file(&safe, &rel, file)? {
            return Err(InstallError::Conflict(format!(
                "{} is not staged",
                file.path
            )));
        }
        let handle = safe.open_file(&rel, Access::Write)?;
        set_executable(&handle, file.executable)
            .map_err(|error| InstallError::io("set permissions of", &path, error))?;
        handle
            .sync_all()
            .map_err(|error| InstallError::io("flush", &path, error))?;
        safe.sync_dir(parent_of(&rel))?;
    }
    for kept in &plan.kept_files {
        let file = new
            .manifest()
            .files
            .get(kept.new_index as usize)
            .ok_or_else(|| InstallError::Conflict("invalid kept file index".into()))?;
        if !verify_file(&safe, &file.path, file)? {
            return Err(InstallError::Conflict(format!("{} is missing", file.path)));
        }
    }
    safe.atomic_write(NEXT_MANIFEST, &new.manifest_bytes)?;
    safe.atomic_write(NEXT_SIGNATURE, &new.envelope.to_bytes())?;
    safe.atomic_write(OLD_MANIFEST, &old.manifest_bytes)?;
    safe.atomic_write(OLD_SIGNATURE, &old.envelope.to_bytes())?;
    safe.sync_dir(STAGING_DIR)?;
    let marker = CommitMarker {
        format: "vgames.commit/1".into(),
        old_digest: old.verified.digest.to_hex(),
        new_digest: new.verified.digest.to_hex(),
        damaged_files: damaged_files.to_vec(),
        mode: CommitMode::Safe,
    };
    let bytes =
        serde_json::to_vec(&marker).map_err(|error| InstallError::Internal(error.to_string()))?;
    safe.atomic_write(&marker_rel(), &bytes)?;
    replay(root, old, new)?.ok_or_else(|| InstallError::Conflict("commit marker vanished".into()))
}

/// Commits an explicitly chosen in-place update after all new file bytes have
/// been verified in their final paths. The install was already unplayable
/// while transferring; the marker makes metadata replacement replayable.
pub fn begin_inplace(
    root: &Path,
    old: &Release,
    new: &Release,
) -> Result<InstallRecord, InstallError> {
    if read_marker(root)?.is_some() {
        return replay(root, old, new)?
            .ok_or_else(|| InstallError::Conflict("commit marker vanished".into()));
    }
    UpdatePlan::new(old, new, true)
        .map_err(|error| InstallError::Conflict(format!("invalid update: {error}")))?;
    ensure_record(root, old, new)?;
    let record =
        read_record(root)?.ok_or_else(|| InstallError::Conflict("missing update record".into()))?;
    if record.state != InstallState::Updating {
        return Err(InstallError::Conflict(
            "in-place update was not started".into(),
        ));
    }
    let safe = SafeRoot::open(root)?;
    if read_installed_manifest(&safe)?
        .ok_or_else(|| InstallError::Conflict("missing old manifest".into()))?
        != old.manifest_bytes
    {
        return Err(InstallError::Conflict(
            "old manifest changed during update".into(),
        ));
    }
    for file in &new.manifest().files {
        let path = safe.path_of(&file.path);
        if !verify_file(&safe, &file.path, file)? {
            return Err(InstallError::Conflict(format!("{} is missing", file.path)));
        }
        let handle = safe.open_file(&file.path, Access::Write)?;
        set_executable(&handle, file.executable)
            .map_err(|error| InstallError::io("set permissions of", &path, error))?;
        handle
            .sync_all()
            .map_err(|error| InstallError::io("flush", &path, error))?;
    }
    for (path, bytes) in [
        (NEXT_MANIFEST, new.manifest_bytes.as_slice()),
        (OLD_MANIFEST, old.manifest_bytes.as_slice()),
    ] {
        safe.atomic_write(path, bytes)?;
    }
    for (path, bytes) in [
        (NEXT_SIGNATURE, new.envelope.to_bytes()),
        (OLD_SIGNATURE, old.envelope.to_bytes()),
    ] {
        safe.atomic_write(path, &bytes)?;
    }
    let marker = CommitMarker {
        format: "vgames.commit/1".into(),
        old_digest: old.verified.digest.to_hex(),
        new_digest: new.verified.digest.to_hex(),
        damaged_files: Vec::new(),
        mode: CommitMode::InPlace,
    };
    let bytes =
        serde_json::to_vec(&marker).map_err(|error| InstallError::Internal(error.to_string()))?;
    safe.atomic_write(&marker_rel(), &bytes)?;
    replay(root, old, new)?.ok_or_else(|| InstallError::Conflict("commit marker vanished".into()))
}

/// Replays a commit after a crash. Returns `None` if no commit was pending.
/// The caller supplies both independently verified signed releases.
pub fn replay(
    root: &Path,
    old: &Release,
    new: &Release,
) -> Result<Option<InstallRecord>, InstallError> {
    let Some(marker) = read_marker(root)? else {
        return Ok(None);
    };
    if marker.old_digest != old.verified.digest.to_hex()
        || marker.new_digest != new.verified.digest.to_hex()
    {
        return Err(InstallError::Conflict(
            "commit marker does not match signed releases".into(),
        ));
    }
    ensure_record(root, old, new)?;
    let plan = UpdatePlan::new_with_damaged(old, new, true, &marker.damaged_files)
        .map_err(|error| InstallError::Conflict(format!("invalid update: {error}")))?;
    let safe = SafeRoot::open(root)?;
    // Delete old files before replacements, allowing file-to-directory layout changes.
    clear_removed(&safe, &plan.remove_files)?;
    match marker.mode {
        CommitMode::Safe => {
            for &index in &plan.build_files {
                let file =
                    new.manifest().files.get(index as usize).ok_or_else(|| {
                        InstallError::Conflict("invalid update file index".into())
                    })?;
                let source = staged(&file.path);
                if verify_file(&safe, &source, file)? {
                    // Replaced through the root's handle; both folders are fsynced.
                    safe.replace(&source, &file.path)?;
                } else if !verify_file(&safe, &file.path, file)? {
                    return Err(InstallError::Conflict(format!(
                        "{} is missing from staging and install",
                        file.path
                    )));
                }
            }
        }
        CommitMode::InPlace => {
            for file in &new.manifest().files {
                if !verify_file(&safe, &file.path, file)? {
                    return Err(InstallError::Conflict(format!("{} is missing", file.path)));
                }
            }
        }
    }
    for dir in &new.manifest().directories {
        safe.ensure_dir(dir)?;
    }
    replace_metadata(&safe, NEXT_MANIFEST, MANIFEST_FILE, &new.manifest_bytes)?;
    replace_metadata(
        &safe,
        NEXT_SIGNATURE,
        SIGNATURE_FILE,
        &new.envelope.to_bytes(),
    )?;
    let mut record = InstallRecord::for_manifest(&new.verified, InstallState::Installed);
    record.installed_at = Some(
        time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
    );
    write_record(safe.root(), &record)?;
    if !safe.remove_file(&marker_rel())? {
        return Err(InstallError::Conflict("commit marker vanished".into()));
    }
    safe.sync_dir(META_DIR)?;
    for name in [OLD_MANIFEST, OLD_SIGNATURE] {
        safe.remove_file(name)?;
    }
    Ok(Some(record))
}

/// Startup entry point: recover a pending commit using its preserved signed
/// releases. Call before reading `manifest.json` or offering Launch.
pub fn recover_pending(
    root: &Path,
    trust: &TrustState,
) -> Result<Option<InstallRecord>, InstallError> {
    let Some(marker) = read_marker(root)? else {
        return Ok(None);
    };
    let safe = SafeRoot::open(root)?;
    let old = load_saved(&safe, OLD_MANIFEST, OLD_SIGNATURE, trust)?;
    let new = load_saved_next(&safe, trust)?;
    if old.verified.digest.to_hex() != marker.old_digest
        || new.verified.digest.to_hex() != marker.new_digest
    {
        return Err(InstallError::Conflict(
            "saved releases do not match the update commit".into(),
        ));
    }
    replay(root, &old, &new)
}

fn load_saved(
    safe: &SafeRoot,
    manifest: &str,
    signature: &str,
    trust: &TrustState,
) -> Result<Release, InstallError> {
    let bytes = safe
        .read(manifest, vgames_core::manifest::MAX_MANIFEST_BYTES as u64)?
        .ok_or_else(|| InstallError::Conflict(format!("missing saved {manifest}")))?;
    let signature_bytes = safe
        .read(signature, MAX_COMMIT_BYTES)?
        .ok_or_else(|| InstallError::Conflict(format!("missing saved {signature}")))?;
    let envelope = Envelope::parse(&signature_bytes)
        .map_err(|error| InstallError::Conflict(format!("invalid saved signature: {error}")))?;
    let parsed = parse_and_validate(&bytes)
        .map_err(|error| InstallError::Conflict(format!("invalid saved manifest: {error}")))?;
    let expected = ExpectedRelease {
        server_id: parsed.server_id,
        package_id: parsed.package_id,
        version_id: parsed.version_id,
        platform: parsed.platform,
        sequence: parsed.sequence,
    };
    Ok(verify_release(
        trust,
        envelope,
        bytes,
        &expected,
        None,
        VerifyMode::Launch,
    )?)
}

fn load_saved_next(safe: &SafeRoot, trust: &TrustState) -> Result<Release, InstallError> {
    let manifest = if safe.existing_file(NEXT_MANIFEST)?.is_some() {
        NEXT_MANIFEST.to_owned()
    } else {
        format!("{META_DIR}/{MANIFEST_FILE}")
    };
    let signature = if safe.existing_file(NEXT_SIGNATURE)?.is_some() {
        NEXT_SIGNATURE.to_owned()
    } else {
        format!("{META_DIR}/{SIGNATURE_FILE}")
    };
    load_saved(safe, &manifest, &signature, trust)
}

fn delete_old_file(safe: &SafeRoot, rel: &str) -> Result<(), InstallError> {
    match safe.remove_file(rel) {
        Ok(true) => Ok(safe.sync_dir(parent_of(rel))?),
        Ok(false) | Err(SafePathError::NotADirectory(_) | SafePathError::NotAFile(_)) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn clear_removed(safe: &SafeRoot, paths: &[String]) -> Result<(), InstallError> {
    for path in paths {
        delete_old_file(safe, path)?;
    }
    for path in paths {
        prune_empty_parents(safe, path)?;
    }
    Ok(())
}

/// The folder of a manifest path (`""` for the root).
fn parent_of(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn prune_empty_parents(safe: &SafeRoot, rel: &str) -> Result<(), InstallError> {
    let mut dir = parent_of(rel);
    while !dir.is_empty() {
        if !safe.remove_empty_dir(dir)? {
            break;
        }
        dir = parent_of(dir);
    }
    Ok(())
}

fn replace_metadata(
    safe: &SafeRoot,
    staged: &str,
    final_name: &str,
    expected: &[u8],
) -> Result<(), InstallError> {
    let target = format!("{META_DIR}/{final_name}");
    let max = vgames_core::manifest::MAX_MANIFEST_BYTES as u64;
    match safe.read(staged, max)? {
        Some(bytes) => {
            if bytes != expected {
                return Err(InstallError::Conflict(format!(
                    "{} changed during commit",
                    safe.path_of(staged).display()
                )));
            }
            safe.replace(staged, &target)?;
        }
        None => {
            if safe.read(&target, max)?.as_deref() != Some(expected) {
                return Err(InstallError::Conflict(format!(
                    "{} does not match the signed release",
                    safe.path_of(&target).display()
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::fsutil;
    use std::fs;
    use std::path::PathBuf;

    fn marker_path(root: &Path) -> PathBuf {
        root.join(META_DIR).join(COMMIT_FILE)
    }
    use crate::testkit::package::{FileSpec, Identity, TestPackage, trust_state, write_tree};
    use uuid::Uuid;
    use vgames_pack::Compression;

    fn setup(
        old_files: &[FileSpec],
        new_files: &[FileSpec],
    ) -> (tempfile::TempDir, Release, Release) {
        let dir = tempfile::tempdir().unwrap();
        let old = TestPackage::build(old_files, &[], Compression::None).release();
        let new = TestPackage::build_with(
            new_files,
            &[],
            Compression::None,
            vgames_pack::PACK_SIZE,
            &Identity {
                version_id: Uuid::now_v7(),
                sequence: 2,
                ..Identity::default()
            },
        )
        .release();
        write_tree(dir.path(), old_files, &[]);
        fs::create_dir(dir.path().join(META_DIR)).unwrap();
        fs::write(
            dir.path().join(META_DIR).join(MANIFEST_FILE),
            &old.manifest_bytes,
        )
        .unwrap();
        fs::write(
            dir.path().join(META_DIR).join(SIGNATURE_FILE),
            old.envelope.to_bytes(),
        )
        .unwrap();
        write_record(
            dir.path(),
            &InstallRecord::for_manifest(&old.verified, InstallState::Installed),
        )
        .unwrap();
        (dir, old, new)
    }

    fn stage(root: &Path, file: &FileSpec) {
        write_tree(&root.join(STAGING_DIR), std::slice::from_ref(file), &[]);
    }

    #[test]
    fn commits_one_changed_file_and_removes_obsolete_file() {
        let old_files = [
            FileSpec::random("keep", 13, 1),
            FileSpec::random("change", 13, 2),
            FileSpec::random("gone", 8, 3),
        ];
        let new_files = [old_files[0].clone(), FileSpec::random("change", 13, 4)];
        let (dir, old, new) = setup(&old_files, &new_files);
        stage(dir.path(), &new_files[1]);
        let record = begin(dir.path(), &old, &new).unwrap();
        assert_eq!(record.version_id, new.manifest().version_id);
        assert_eq!(
            fs::read(dir.path().join("change")).unwrap(),
            new_files[1].bytes()
        );
        assert!(!dir.path().join("gone").exists());
        assert_eq!(
            fs::read(dir.path().join("keep")).unwrap(),
            old_files[0].bytes()
        );
        assert!(replay(dir.path(), &old, &new).unwrap().is_none());
    }

    #[test]
    fn replay_after_partial_rename_completes_idempotently() {
        let old_files = [FileSpec::random("old/a", 8, 1)];
        let new_files = [FileSpec::random("old", 8, 2)];
        let (dir, old, new) = setup(&old_files, &new_files);
        stage(dir.path(), &new_files[0]);
        let marker = CommitMarker {
            format: "vgames.commit/1".into(),
            old_digest: old.verified.digest.to_hex(),
            new_digest: new.verified.digest.to_hex(),
            damaged_files: Vec::new(),
            mode: CommitMode::Safe,
        };
        fsutil::atomic_write(
            &dir.path()
                .join(META_DIR)
                .join(NEXT_MANIFEST.rsplit('/').next().unwrap()),
            &new.manifest_bytes,
        )
        .unwrap();
        fsutil::atomic_write(
            &dir.path()
                .join(META_DIR)
                .join(NEXT_SIGNATURE.rsplit('/').next().unwrap()),
            &new.envelope.to_bytes(),
        )
        .unwrap();
        fsutil::atomic_write(&dir.path().join(OLD_MANIFEST), &old.manifest_bytes).unwrap();
        fsutil::atomic_write(&dir.path().join(OLD_SIGNATURE), &old.envelope.to_bytes()).unwrap();
        fsutil::atomic_write(
            &marker_path(dir.path()),
            &serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        // Simulate a crash after the old file was deleted.
        fs::remove_file(dir.path().join("old/a")).unwrap();
        fs::remove_dir(dir.path().join("old")).unwrap();
        fs::rename(
            dir.path().join(NEXT_MANIFEST),
            dir.path().join(META_DIR).join(MANIFEST_FILE),
        )
        .unwrap();
        let record = recover_pending(dir.path(), &trust_state())
            .unwrap()
            .unwrap();
        assert_eq!(record.state, InstallState::Installed);
        assert_eq!(
            fs::read(dir.path().join("old")).unwrap(),
            new_files[0].bytes()
        );
    }

    #[test]
    fn refuses_bad_stage_without_changing_install() {
        let old_files = [FileSpec::random("game", 8, 1)];
        let new_files = [FileSpec::random("game", 8, 2)];
        let (dir, old, new) = setup(&old_files, &new_files);
        stage(dir.path(), &old_files[0]);
        assert!(begin(dir.path(), &old, &new).is_err());
        assert_eq!(
            fs::read(dir.path().join("game")).unwrap(),
            old_files[0].bytes()
        );
        assert!(!marker_path(dir.path()).exists());
    }

    #[test]
    fn repair_marker_replays_damaged_file_after_restart() {
        let files = [
            FileSpec::random("bad.bin", 16, 1),
            FileSpec::random("good.bin", 16, 2),
        ];
        let package = TestPackage::build(&files, &[], Compression::None);
        let release = package.release();
        let dir = tempfile::tempdir().unwrap();
        write_tree(dir.path(), &files, &[]);
        fs::create_dir(dir.path().join(META_DIR)).unwrap();
        fs::write(
            dir.path().join(META_DIR).join(MANIFEST_FILE),
            &release.manifest_bytes,
        )
        .unwrap();
        fs::write(
            dir.path().join(META_DIR).join(SIGNATURE_FILE),
            release.envelope.to_bytes(),
        )
        .unwrap();
        write_record(
            dir.path(),
            &InstallRecord::for_manifest(&release.verified, InstallState::Installed),
        )
        .unwrap();
        fs::write(dir.path().join("bad.bin"), b"corrupt contents").unwrap();
        stage(dir.path(), &files[0]);
        for (path, bytes) in [
            (NEXT_MANIFEST, release.manifest_bytes.as_slice()),
            (OLD_MANIFEST, release.manifest_bytes.as_slice()),
        ] {
            fsutil::atomic_write(&dir.path().join(path), bytes).unwrap();
        }
        for path in [NEXT_SIGNATURE, OLD_SIGNATURE] {
            fsutil::atomic_write(&dir.path().join(path), &release.envelope.to_bytes()).unwrap();
        }
        let marker = CommitMarker {
            format: "vgames.commit/1".into(),
            old_digest: release.verified.digest.to_hex(),
            new_digest: release.verified.digest.to_hex(),
            damaged_files: vec![0],
            mode: CommitMode::Safe,
        };
        fsutil::atomic_write(
            &marker_path(dir.path()),
            &serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        let recovered = recover_pending(dir.path(), &trust_state())
            .unwrap()
            .unwrap();
        assert_eq!(recovered.version_id, release.manifest().version_id);
        assert_eq!(
            fs::read(dir.path().join("bad.bin")).unwrap(),
            files[0].bytes()
        );
        assert_eq!(
            fs::read(dir.path().join("good.bin")).unwrap(),
            files[1].bytes()
        );
    }

    #[test]
    fn inplace_marker_replays_after_partial_metadata_replacement() {
        let old_files = [FileSpec::random("game.bin", 32, 1)];
        let new_files = [FileSpec::random("game.bin", 32, 2)];
        let (dir, old, new) = setup(&old_files, &new_files);
        fs::write(dir.path().join("game.bin"), new_files[0].bytes()).unwrap();
        write_record(
            dir.path(),
            &InstallRecord::for_manifest(&old.verified, InstallState::Updating),
        )
        .unwrap();
        for (path, bytes) in [
            (NEXT_MANIFEST, new.manifest_bytes.as_slice()),
            (OLD_MANIFEST, old.manifest_bytes.as_slice()),
        ] {
            fsutil::atomic_write(&dir.path().join(path), bytes).unwrap();
        }
        for (path, bytes) in [
            (NEXT_SIGNATURE, new.envelope.to_bytes()),
            (OLD_SIGNATURE, old.envelope.to_bytes()),
        ] {
            fsutil::atomic_write(&dir.path().join(path), &bytes).unwrap();
        }
        let marker = CommitMarker {
            format: "vgames.commit/1".into(),
            old_digest: old.verified.digest.to_hex(),
            new_digest: new.verified.digest.to_hex(),
            damaged_files: Vec::new(),
            mode: CommitMode::InPlace,
        };
        fsutil::atomic_write(
            &marker_path(dir.path()),
            &serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        fs::rename(
            dir.path().join(NEXT_MANIFEST),
            dir.path().join(META_DIR).join(MANIFEST_FILE),
        )
        .unwrap();
        let record = recover_pending(dir.path(), &trust_state())
            .unwrap()
            .unwrap();
        assert_eq!(record.version_id, new.manifest().version_id);
        assert_eq!(record.state, InstallState::Installed);
        assert_eq!(
            fs::read(dir.path().join("game.bin")).unwrap(),
            new_files[0].bytes()
        );
        assert!(!marker_path(dir.path()).exists());
    }
}
