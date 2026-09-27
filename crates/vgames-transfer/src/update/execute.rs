//! Safe-mode update transfer: reuse verified old chunks, fetch only remaining
//! changed-file chunks into staging, then commit the staged release.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::commit;
use super::plan::{Source, UpdatePlan};
use crate::download::journal::{Bitset, Journal, JournalKey};
use crate::download::table::ChunkTable;
use crate::download::{
    self, DownloadControl, DownloadOptions, DownloadSpec, PackUrlSource, Phase, RunOutcome,
};
use crate::fsutil::{self, SafeRoot};
use crate::install::{
    self, InstallError, InstallOutcome, InstallReport, InstallState, META_DIR, Release,
    SPACE_MARGIN, read_record,
};
use crate::sys;

const STAGING: &str = ".vgames/staging";
const UPDATE_JOURNAL: &str = ".vgames/update-journal.bin";

/// Updates an installed release in safe mode. Both releases must have passed
/// `verify_manifest` under current trust. A paused transfer is resumed by
/// calling this function again with the same releases and root.
pub async fn update_safe<A: PackUrlSource>(
    root: &Path,
    old: Arc<Release>,
    new: Arc<Release>,
    api: Arc<A>,
    options: &DownloadOptions,
    control: &DownloadControl,
    allow_older: bool,
) -> Result<InstallReport, InstallError> {
    run_safe(
        root,
        old,
        new,
        api,
        options,
        control,
        allow_older,
        Vec::new(),
    )
    .await
}

/// Rebuilds files reported by `verify_install` without changing the release.
/// Damaged old chunks are never reused as repair sources.
pub async fn repair_safe<A: PackUrlSource>(
    root: &Path,
    release: Arc<Release>,
    damaged_files: &[u32],
    api: Arc<A>,
    options: &DownloadOptions,
    control: &DownloadControl,
) -> Result<InstallReport, InstallError> {
    run_safe(
        root,
        Arc::clone(&release),
        release,
        api,
        options,
        control,
        true,
        damaged_files.to_vec(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_safe<A: PackUrlSource>(
    root: &Path,
    old: Arc<Release>,
    new: Arc<Release>,
    api: Arc<A>,
    options: &DownloadOptions,
    control: &DownloadControl,
    allow_older: bool,
    damaged_files: Vec<u32>,
) -> Result<InstallReport, InstallError> {
    control.set_phase(Phase::Allocating);
    let plan = UpdatePlan::new_with_damaged(&old, &new, allow_older, &damaged_files)
        .map_err(|error| InstallError::Conflict(format!("invalid update: {error}")))?;
    let prepared = {
        let root = root.to_owned();
        let old = Arc::clone(&old);
        let new = Arc::clone(&new);
        let plan = plan.clone();
        let control = control.clone();
        tokio::task::spawn_blocking(move || prepare(&root, &old, &new, &plan, &control))
            .await
            .map_err(|error| InstallError::Internal(format!("update prepare: {error}")))??
    };
    let spec = DownloadSpec {
        table: prepared.table,
        targets: prepared.targets,
        journal: prepared.journal,
        wanted: Some(prepared.wanted),
    };
    let report = download::run(spec, api, options, control).await?;
    let outcome = match report.outcome {
        RunOutcome::Completed => {
            control.set_phase(Phase::Finalizing);
            let root = root.to_owned();
            let record = tokio::task::spawn_blocking(move || {
                let record = commit::begin_with_damaged(&root, &old, &new, &damaged_files)?;
                let journal = root.join(UPDATE_JOURNAL);
                if let Err(error) = fs::remove_file(&journal)
                    && error.kind() != io::ErrorKind::NotFound
                {
                    tracing::warn!(%error, "cannot remove completed update journal");
                }
                let staging = root.join(STAGING);
                if let Err(error) = install::remove_tree_no_follow(&staging) {
                    tracing::warn!(%error, "cannot remove completed update staging");
                }
                Ok::<_, InstallError>(record)
            })
            .await
            .map_err(|error| InstallError::Internal(format!("update commit: {error}")))??;
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

struct Prepared {
    table: Arc<ChunkTable>,
    targets: Arc<Vec<Option<PathBuf>>>,
    journal: Journal,
    wanted: Bitset,
}

fn prepare(
    root: &Path,
    old: &Release,
    new: &Release,
    plan: &UpdatePlan,
    control: &DownloadControl,
) -> Result<Prepared, InstallError> {
    if commit::is_pending(root)? {
        return Err(InstallError::Conflict(
            "finish the interrupted update commit before downloading again".into(),
        ));
    }
    let old_record = read_record(root)?
        .ok_or_else(|| InstallError::Conflict("missing installed release".into()))?;
    if old_record.version_id != old.manifest().version_id
        || old_record.manifest_blake3 != old.verified.digest.to_hex()
        || old_record.server_id != old.manifest().server_id
        || old_record.package_id != old.manifest().package_id
        || old_record.state != InstallState::Installed
    {
        return Err(InstallError::Conflict(
            "installed release changed before update".into(),
        ));
    }
    let safe = SafeRoot::open(root)?;
    if let Some(limit) = sys::max_file_size(safe.root())
        .map_err(|error| InstallError::io("inspect", safe.root(), error))?
        && let Some(file) = plan
            .build_files
            .iter()
            .filter_map(|&index| new.manifest().files.get(index as usize))
            .find(|file| file.size > limit)
    {
        return Err(InstallError::FileTooLarge {
            path: file.path.clone(),
            size: file.size,
            limit,
        });
    }
    let stage_dir = safe.ensure_dir(STAGING)?;
    let stage = SafeRoot::open(&stage_dir)?;
    let mut targets = vec![None; new.manifest().files.len()];
    let mut additional = 0u64;
    let mut reset_journal = false;
    for &index in &plan.build_files {
        if control.is_cancelled() {
            return Err(InstallError::Cancelled);
        }
        let file = new
            .manifest()
            .files
            .get(index as usize)
            .ok_or_else(|| InstallError::Conflict("invalid build file index".into()))?;
        let path = stage.file_target(&file.path)?;
        let previous = fs::symlink_metadata(&path).ok().map(|meta| meta.len());
        if previous != Some(file.size) {
            reset_journal = true;
        }
        additional = additional
            .checked_add(file.size.saturating_sub(previous.unwrap_or(0)))
            .ok_or_else(|| InstallError::Internal("update size overflow".into()))?;
        if let Some(slot) = targets.get_mut(index as usize) {
            *slot = Some(path);
        }
    }
    let available = sys::available_space(safe.root())
        .map_err(|error| InstallError::io("inspect", safe.root(), error))?;
    let required = additional.saturating_add(SPACE_MARGIN);
    if available < required {
        return Err(InstallError::NotEnoughSpace {
            required,
            available,
        });
    }
    let journal_path = safe.path_of(UPDATE_JOURNAL);
    if reset_journal {
        match fs::remove_file(&journal_path) {
            Ok(()) => fsutil::sync_dir(&safe.path_of(META_DIR))
                .map_err(|error| InstallError::io("flush", &journal_path, error))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(InstallError::io("delete", &journal_path, error)),
        }
    }
    for (index, target) in targets.iter().enumerate() {
        let Some(target) = target else {
            continue;
        };
        let file = new
            .manifest()
            .files
            .get(index)
            .ok_or_else(|| InstallError::Internal("staging target lost".into()))?;
        let mut options = OpenOptions::new();
        options.write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let handle = options
            .open(target)
            .map_err(|error| InstallError::io("create", target, error))?;
        if let Err(error) = sys::preallocate(&handle, file.size) {
            if sys::is_disk_full(&error) {
                return Err(InstallError::NotEnoughSpace {
                    required,
                    available: sys::available_space(safe.root()).unwrap_or(0),
                });
            }
            return Err(InstallError::io("allocate", target, error));
        }
    }
    let table = Arc::new(ChunkTable::new(new.manifest())?);
    let old_table = ChunkTable::new(old.manifest())?;
    let mut journal = Journal::load_or_new(
        &journal_path,
        JournalKey {
            version_id: new.manifest().version_id,
            manifest_blake3: *new.verified.digest.as_bytes(),
            chunk_count: table.len(),
        },
    );
    let mut wanted = Bitset::new(table.len());
    let mut touched = Vec::new();
    for needed in &plan.needed_chunks {
        wanted.set(needed.new_chunk);
        if journal.done().get(needed.new_chunk) {
            continue;
        }
        if let Source::Local { old_chunk } = needed.source
            && let Some(bytes) = read_old_chunk(&safe, old.manifest(), &old_table, old_chunk)?
        {
            write_chunk(&table, &targets, needed.new_chunk, &bytes, &mut touched)?;
            journal.mark(needed.new_chunk);
        }
    }
    touched.sort();
    touched.dedup();
    for path in touched {
        fsutil::open_for_write(&path)
            .and_then(|handle| handle.sync_all())
            .map_err(|error| InstallError::io("flush", &path, error))?;
    }
    journal
        .persist()
        .map_err(|error| InstallError::io("write", &journal_path, error))?;
    Ok(Prepared {
        table,
        targets: Arc::new(targets),
        journal,
        wanted,
    })
}

pub(super) fn read_old_chunk(
    safe: &SafeRoot,
    manifest: &vgames_core::manifest::Manifest,
    table: &ChunkTable,
    index: u32,
) -> Result<Option<Vec<u8>>, InstallError> {
    let chunk = table
        .chunk(index)
        .ok_or_else(|| InstallError::Conflict("invalid local chunk".into()))?;
    let size = usize::try_from(chunk.size)
        .map_err(|_| InstallError::Conflict("local chunk too large".into()))?;
    let mut bytes = vec![0u8; size];
    let mut at = 0usize;
    for extent in table.extents(index) {
        let old_file = manifest
            .files
            .get(extent.file as usize)
            .ok_or_else(|| InstallError::Conflict("invalid old file index".into()))?;
        let file = safe.existing_file(&old_file.path)?;
        let Some(path) = file else {
            return Ok(None);
        };
        let handle = match fsutil::open_for_read(&path) {
            Ok(handle) => handle,
            Err(_) => return Ok(None),
        };
        let len = usize::try_from(extent.len)
            .map_err(|_| InstallError::Conflict("local extent too large".into()))?;
        let end = at
            .checked_add(len)
            .ok_or_else(|| InstallError::Conflict("local chunk overflow".into()))?;
        let part = bytes
            .get_mut(at..end)
            .ok_or_else(|| InstallError::Conflict("local chunk layout".into()))?;
        if fsutil::read_exact_at(&handle, part, extent.file_offset).is_err() {
            return Ok(None);
        }
        at = end;
    }
    if blake3::hash(&bytes).as_bytes() != &chunk.blake3 {
        return Ok(None);
    }
    Ok(Some(bytes))
}

fn write_chunk(
    table: &ChunkTable,
    targets: &[Option<PathBuf>],
    index: u32,
    bytes: &[u8],
    touched: &mut Vec<PathBuf>,
) -> Result<(), InstallError> {
    let mut at = 0usize;
    for extent in table.extents(index) {
        let len = usize::try_from(extent.len)
            .map_err(|_| InstallError::Conflict("new extent too large".into()))?;
        let end = at
            .checked_add(len)
            .ok_or_else(|| InstallError::Conflict("new chunk overflow".into()))?;
        let part = bytes
            .get(at..end)
            .ok_or_else(|| InstallError::Conflict("new chunk layout".into()))?;
        if let Some(Some(path)) = targets.get(extent.file as usize) {
            let handle = fsutil::open_for_write(path)
                .map_err(|error| InstallError::io("open", path, error))?;
            fsutil::write_all_at(&handle, part, extent.file_offset)
                .map_err(|error| InstallError::io("write", path, error))?;
            touched.push(path.clone());
        }
        at = end;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::install::{self, InstallRecord, InstallState};
    use crate::testkit::package::{FileSpec, Identity, TestPackage, write_tree};
    use crate::testkit::{MockApi, Rig};
    use uuid::Uuid;
    use vgames_pack::Compression;

    fn packages(old_files: &[FileSpec], new_files: &[FileSpec]) -> (TestPackage, TestPackage) {
        let old = TestPackage::build(old_files, &[], Compression::None);
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
        );
        (old, new)
    }

    fn install_old(root: &Path, package: &TestPackage) {
        write_tree(root, &package.files, &[]);
        fs::create_dir(root.join(META_DIR)).unwrap();
        fs::write(root.join(META_DIR).join("manifest.json"), &package.manifest).unwrap();
        fs::write(
            root.join(META_DIR).join("manifest.sig"),
            package.envelope.to_bytes(),
        )
        .unwrap();
        install::write_record(
            root,
            &InstallRecord::for_manifest(&package.release().verified, InstallState::Installed),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn one_changed_file_of_ten_thousand_fetches_only_its_chunk() {
        let files = (0..10_000)
            .map(|i| FileSpec::random(&format!("file-{i:05}.bin"), 100, i))
            .collect::<Vec<_>>();
        let mut changed = files.clone();
        changed[4_999] = FileSpec::random("file-04999.bin", 100, 999_999);
        let (old, new) = packages(&files, &changed);
        let dir = tempfile::tempdir().unwrap();
        install_old(dir.path(), &old);
        let rig = Rig::start(new.manifest.clone(), new.packs.clone()).await;
        let api = MockApi::new(&rig, new.packs.iter().map(|p| p.len() as u64).collect());
        let report = update_safe(
            dir.path(),
            Arc::new(old.release()),
            Arc::new(new.release()),
            api,
            &DownloadOptions::default(),
            &DownloadControl::new(None),
            false,
        )
        .await
        .unwrap();
        assert!(matches!(report.outcome, InstallOutcome::Installed(_)));
        assert_eq!(rig.pack_bytes(), 1_000_000);
        assert_eq!(
            fs::read(dir.path().join("file-04999.bin")).unwrap(),
            changed[4_999].bytes()
        );
        assert_eq!(
            fs::read(dir.path().join("file-04998.bin")).unwrap(),
            files[4_998].bytes()
        );
    }

    #[tokio::test]
    async fn identical_chunk_at_new_path_is_reused_without_network() {
        let old_files = [FileSpec::random("old.bin", 4 * 1024 * 1024, 3)];
        let new_files = [FileSpec::random("new.bin", 4 * 1024 * 1024, 3)];
        let (old, new) = packages(&old_files, &new_files);
        let dir = tempfile::tempdir().unwrap();
        install_old(dir.path(), &old);
        let rig = Rig::start(new.manifest.clone(), new.packs.clone()).await;
        let api = MockApi::new(&rig, new.packs.iter().map(|p| p.len() as u64).collect());
        let report = update_safe(
            dir.path(),
            Arc::new(old.release()),
            Arc::new(new.release()),
            api,
            &DownloadOptions::default(),
            &DownloadControl::new(None),
            false,
        )
        .await
        .unwrap();
        assert!(matches!(report.outcome, InstallOutcome::Installed(_)));
        assert_eq!(rig.pack_bytes(), 0);
        assert!(!dir.path().join("old.bin").exists());
        assert_eq!(
            fs::read(dir.path().join("new.bin")).unwrap(),
            new_files[0].bytes()
        );
    }

    #[tokio::test]
    async fn damaged_local_candidate_is_fetched_from_network() {
        let old_files = [FileSpec::random("old.bin", 4 * 1024 * 1024, 3)];
        let new_files = [FileSpec::random("new.bin", 4 * 1024 * 1024, 3)];
        let (old, new) = packages(&old_files, &new_files);
        let dir = tempfile::tempdir().unwrap();
        install_old(dir.path(), &old);
        fs::write(dir.path().join("old.bin"), vec![0u8; 4 * 1024 * 1024]).unwrap();
        let rig = Rig::start(new.manifest.clone(), new.packs.clone()).await;
        let api = MockApi::new(&rig, new.packs.iter().map(|p| p.len() as u64).collect());
        let report = update_safe(
            dir.path(),
            Arc::new(old.release()),
            Arc::new(new.release()),
            api,
            &DownloadOptions::default(),
            &DownloadControl::new(None),
            false,
        )
        .await
        .unwrap();
        assert!(matches!(report.outcome, InstallOutcome::Installed(_)));
        assert_eq!(rig.pack_bytes(), 4 * 1024 * 1024);
        assert_eq!(
            fs::read(dir.path().join("new.bin")).unwrap(),
            new_files[0].bytes()
        );
    }

    #[tokio::test]
    async fn paused_update_resumes_from_staging() {
        let old_files = [FileSpec::random("game.bin", 1024, 1)];
        let new_files = [FileSpec::random("game.bin", 1024, 2)];
        let (old, new) = packages(&old_files, &new_files);
        let dir = tempfile::tempdir().unwrap();
        install_old(dir.path(), &old);
        let rig = Rig::start(new.manifest.clone(), new.packs.clone()).await;
        let api = MockApi::new(&rig, new.packs.iter().map(|p| p.len() as u64).collect());
        let control = DownloadControl::new(None);
        control.pause();
        let paused = update_safe(
            dir.path(),
            Arc::new(old.release()),
            Arc::new(new.release()),
            Arc::clone(&api),
            &DownloadOptions::default(),
            &control,
            false,
        )
        .await
        .unwrap();
        assert!(matches!(paused.outcome, InstallOutcome::Paused(_)));
        assert_eq!(
            fs::read(dir.path().join("game.bin")).unwrap(),
            old_files[0].bytes()
        );
        control.resume();
        let completed = update_safe(
            dir.path(),
            Arc::new(old.release()),
            Arc::new(new.release()),
            api,
            &DownloadOptions::default(),
            &control,
            false,
        )
        .await
        .unwrap();
        assert!(matches!(completed.outcome, InstallOutcome::Installed(_)));
        assert_eq!(
            fs::read(dir.path().join("game.bin")).unwrap(),
            new_files[0].bytes()
        );
    }

    #[tokio::test]
    async fn repair_rebuilds_damaged_file_without_changing_version() {
        let files = [
            FileSpec::random("good.bin", 1024, 1),
            FileSpec::random("bad.bin", 1024, 2),
        ];
        let package = TestPackage::build(&files, &[], Compression::None);
        let dir = tempfile::tempdir().unwrap();
        install_old(dir.path(), &package);
        fs::write(dir.path().join("bad.bin"), vec![0u8; 1024]).unwrap();
        let rig = Rig::start(package.manifest.clone(), package.packs.clone()).await;
        let api = MockApi::new(
            &rig,
            package.packs.iter().map(|pack| pack.len() as u64).collect(),
        );
        let report = repair_safe(
            dir.path(),
            Arc::new(package.release()),
            &[0],
            api,
            &DownloadOptions::default(),
            &DownloadControl::new(None),
        )
        .await
        .unwrap();
        assert!(matches!(report.outcome, InstallOutcome::Installed(_)));
        assert_eq!(
            fs::read(dir.path().join("bad.bin")).unwrap(),
            files[1].bytes()
        );
        assert_eq!(
            fs::read(dir.path().join("good.bin")).unwrap(),
            files[0].bytes()
        );
        assert_eq!(
            install::read_record(dir.path())
                .unwrap()
                .unwrap()
                .version_id,
            package.expected.version_id
        );
        assert_eq!(rig.pack_bytes(), 2048);
    }
}
