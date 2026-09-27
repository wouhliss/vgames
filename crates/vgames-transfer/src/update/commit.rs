//! Durable, replayable safe-update commit. The caller stages and verifies all
//! changed files before this module writes `commit.json`; after that point a
//! restart must call [`recover_pending`] before exposing the install as playable.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use super::plan::UpdatePlan;
use crate::fsutil::{self, SafePathError, SafeRoot};
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
}

fn marker_path(root: &Path) -> PathBuf {
    root.join(META_DIR).join(COMMIT_FILE)
}

fn read_marker(root: &Path) -> Result<Option<CommitMarker>, InstallError> {
    let safe = SafeRoot::open(root)?;
    let Some(path) = safe.existing_file(&format!("{META_DIR}/{COMMIT_FILE}"))? else {
        return Ok(None);
    };
    match fs::symlink_metadata(&path) {
        Ok(meta)
            if !meta.is_file()
                || meta.file_type().is_symlink()
                || meta.len() > MAX_COMMIT_BYTES =>
        {
            return Err(InstallError::Conflict(
                "invalid update commit marker".into(),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(InstallError::io("inspect", &path, error)),
    }
    let bytes = fs::read(&path).map_err(|error| InstallError::io("read", &path, error))?;
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

fn verify_file(path: &Path, expected: &vgames_core::manifest::File) -> Result<(), InstallError> {
    let meta =
        fs::symlink_metadata(path).map_err(|error| InstallError::io("inspect", path, error))?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() != expected.size {
        return Err(InstallError::Conflict(format!(
            "{} does not match the signed manifest",
            path.display()
        )));
    }
    let mut handle =
        fsutil::open_for_read(path).map_err(|error| InstallError::io("open", path, error))?;
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
    Ok(())
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
    let current = safe
        .existing_file(&format!("{META_DIR}/{MANIFEST_FILE}"))?
        .ok_or_else(|| InstallError::Conflict("missing installed manifest".into()))?;
    if fs::read(&current).map_err(|error| InstallError::io("read", &current, error))?
        != old.manifest_bytes
    {
        return Err(InstallError::Conflict(
            "installed manifest changed before commit".into(),
        ));
    }
    let staging = SafeRoot::open(&safe.path_of(STAGING_DIR))?;
    for &index in &plan.build_files {
        let file = new
            .manifest()
            .files
            .get(index as usize)
            .ok_or_else(|| InstallError::Conflict("invalid update file index".into()))?;
        let path = staging
            .existing_file(&file.path)?
            .ok_or_else(|| InstallError::Conflict(format!("{} is not staged", file.path)))?;
        verify_file(&path, file)?;
        let handle = fsutil::open_for_write(&path)
            .map_err(|error| InstallError::io("open", &path, error))?;
        set_executable(&handle, file.executable)
            .map_err(|error| InstallError::io("set permissions of", &path, error))?;
        handle
            .sync_all()
            .map_err(|error| InstallError::io("flush", &path, error))?;
        if let Some(parent) = path.parent() {
            fsutil::sync_dir(parent).map_err(|error| InstallError::io("flush", parent, error))?;
        }
    }
    for kept in &plan.kept_files {
        let file = new
            .manifest()
            .files
            .get(kept.new_index as usize)
            .ok_or_else(|| InstallError::Conflict("invalid kept file index".into()))?;
        let path = safe
            .existing_file(&file.path)?
            .ok_or_else(|| InstallError::Conflict(format!("{} is missing", file.path)))?;
        verify_file(&path, file)?;
    }
    let meta = safe.ensure_dir(META_DIR)?;
    fsutil::atomic_write(&safe.path_of(NEXT_MANIFEST), &new.manifest_bytes)
        .map_err(|error| InstallError::io("write", &meta, error))?;
    fsutil::atomic_write(&safe.path_of(NEXT_SIGNATURE), &new.envelope.to_bytes())
        .map_err(|error| InstallError::io("write", &meta, error))?;
    fsutil::atomic_write(&safe.path_of(OLD_MANIFEST), &old.manifest_bytes)
        .map_err(|error| InstallError::io("write", &meta, error))?;
    fsutil::atomic_write(&safe.path_of(OLD_SIGNATURE), &old.envelope.to_bytes())
        .map_err(|error| InstallError::io("write", &meta, error))?;
    fsutil::sync_dir(staging.root())
        .map_err(|error| InstallError::io("flush", staging.root(), error))?;
    let marker = CommitMarker {
        format: "vgames.commit/1".into(),
        old_digest: old.verified.digest.to_hex(),
        new_digest: new.verified.digest.to_hex(),
        damaged_files: damaged_files.to_vec(),
    };
    let bytes =
        serde_json::to_vec(&marker).map_err(|error| InstallError::Internal(error.to_string()))?;
    fsutil::atomic_write(&marker_path(safe.root()), &bytes)
        .map_err(|error| InstallError::io("write", &marker_path(safe.root()), error))?;
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
    let staging = SafeRoot::open(&safe.path_of(STAGING_DIR))?;
    // Delete old files before replacements, allowing file-to-directory layout changes.
    for path in &plan.remove_files {
        delete_old_file(&safe, path)?;
    }
    for path in &plan.remove_files {
        prune_empty_parents(&safe, path)?;
    }
    for &index in &plan.build_files {
        let file = new
            .manifest()
            .files
            .get(index as usize)
            .ok_or_else(|| InstallError::Conflict("invalid update file index".into()))?;
        let source = staging.existing_file(&file.path)?;
        if let Some(source) = source {
            verify_file(&source, file)?;
            let target = safe.file_target(&file.path)?;
            replace_file(&source, &target)?;
            if let Some(parent) = source.parent() {
                fsutil::sync_dir(parent)
                    .map_err(|error| InstallError::io("flush", parent, error))?;
            }
            if let Some(parent) = target.parent() {
                fsutil::sync_dir(parent)
                    .map_err(|error| InstallError::io("flush", parent, error))?;
            }
        } else {
            let target = safe.existing_file(&file.path)?.ok_or_else(|| {
                InstallError::Conflict(format!("{} is missing from staging and install", file.path))
            })?;
            verify_file(&target, file)?;
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
    fs::remove_file(marker_path(safe.root()))
        .map_err(|error| InstallError::io("delete", &marker_path(safe.root()), error))?;
    fsutil::sync_dir(&safe.path_of(META_DIR))
        .map_err(|error| InstallError::io("flush", &safe.path_of(META_DIR), error))?;
    for name in [OLD_MANIFEST, OLD_SIGNATURE] {
        let path = safe.path_of(name);
        fs::remove_file(&path).map_err(|error| InstallError::io("delete", &path, error))?;
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
    let manifest_path = safe
        .existing_file(manifest)?
        .ok_or_else(|| InstallError::Conflict(format!("missing saved {manifest}")))?;
    let signature_path = safe
        .existing_file(signature)?
        .ok_or_else(|| InstallError::Conflict(format!("missing saved {signature}")))?;
    let bytes = fs::read(&manifest_path)
        .map_err(|error| InstallError::io("read", &manifest_path, error))?;
    let signature_bytes = fs::read(&signature_path)
        .map_err(|error| InstallError::io("read", &signature_path, error))?;
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
    let existing = match safe.existing_file(rel) {
        Ok(path) => path,
        Err(SafePathError::NotADirectory(_)) => None, // an earlier replay installed a file here
        Err(error) => return Err(error.into()),
    };
    if let Some(path) = existing {
        fs::remove_file(&path).map_err(|error| InstallError::io("delete", &path, error))?;
        if let Some(parent) = path.parent() {
            fsutil::sync_dir(parent).map_err(|error| InstallError::io("flush", parent, error))?;
        }
    }
    Ok(())
}

fn prune_empty_parents(safe: &SafeRoot, rel: &str) -> Result<(), InstallError> {
    let mut path = safe.path_of(rel);
    while let Some(parent) = path.parent() {
        if parent == safe.root() {
            break;
        }
        match fs::symlink_metadata(parent) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(SafePathError::Link(parent.to_owned()).into());
            }
            Ok(meta) if meta.is_dir() => match fs::remove_dir(parent) {
                Ok(()) => {
                    if let Some(grandparent) = parent.parent() {
                        fsutil::sync_dir(grandparent)
                            .map_err(|error| InstallError::io("flush", grandparent, error))?;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::NotFound
                    ) =>
                {
                    break;
                }
                Err(error) => return Err(InstallError::io("delete", parent, error)),
            },
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(InstallError::io("inspect", parent, error)),
        }
        path = parent.to_owned();
    }
    Ok(())
}

fn replace_metadata(
    safe: &SafeRoot,
    staged: &str,
    final_name: &str,
    expected: &[u8],
) -> Result<(), InstallError> {
    let source = safe.path_of(staged);
    let target = safe.path_of(&format!("{META_DIR}/{final_name}"));
    match safe.existing_file(staged)? {
        Some(_) => {
            if fs::read(&source).map_err(|error| InstallError::io("read", &source, error))?
                != expected
            {
                return Err(InstallError::Conflict(format!(
                    "{} changed during commit",
                    source.display()
                )));
            }
            replace_file(&source, &target)?;
            fsutil::sync_dir(&safe.path_of(META_DIR))
                .map_err(|error| InstallError::io("flush", &target, error))?;
        }
        None => {
            if fs::read(&target).map_err(|error| InstallError::io("read", &target, error))?
                != expected
            {
                return Err(InstallError::Conflict(format!(
                    "{} does not match the signed release",
                    target.display()
                )));
            }
        }
    }
    Ok(())
}

fn replace_file(source: &Path, target: &Path) -> Result<(), InstallError> {
    // Windows rename refuses an existing target. The commit marker makes the
    // delete/rename gap replayable if the process stops between these calls.
    #[cfg(windows)]
    match fs::remove_file(target) {
        Ok(()) => {
            if let Some(parent) = target.parent() {
                fsutil::sync_dir(parent)
                    .map_err(|error| InstallError::io("flush", parent, error))?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(InstallError::io("delete", target, error)),
    }
    fs::rename(source, target).map_err(|error| InstallError::io("replace", target, error))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
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
}
