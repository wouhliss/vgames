//! Explicit low-space update mode. Changed files are written at their final
//! paths, so the install stays marked `updating` until commit.

use std::path::Path;
use std::sync::Arc;

use super::plan::UpdatePlan;
use super::{commit, execute};
use crate::download::journal::{Bitset, Journal, JournalKey};
use crate::download::table::ChunkTable;
use crate::download::{
    self, DownloadControl, DownloadOptions, DownloadSpec, PackUrlSource, Phase, RunOutcome,
};
use crate::fsutil::{Access, SafePathError, SafeRoot, Target};
use crate::install::{
    InstallError, InstallOutcome, InstallReport, InstallState, MANIFEST_FILE, META_DIR, Release,
    SPACE_MARGIN, read_record, write_record_in,
};
use crate::sys;
use vgames_core::manifest::MAX_MANIFEST_BYTES;

const JOURNAL: &str = ".vgames/inplace-journal.bin";
const NEXT_MANIFEST: &str = ".vgames/next-manifest.json";
const NEXT_SIGNATURE: &str = ".vgames/next-manifest.sig";

/// The caller must have shown a warning before setting `explicit_choice`.
/// Retry with the same releases to resume a paused or interrupted update.
#[allow(clippy::too_many_arguments)]
pub async fn update_in_place<A: PackUrlSource>(
    root: &Path,
    old: Arc<Release>,
    new: Arc<Release>,
    api: Arc<A>,
    options: &DownloadOptions,
    control: &DownloadControl,
    allow_older: bool,
    explicit_choice: bool,
) -> Result<InstallReport, InstallError> {
    if !explicit_choice {
        return Err(InstallError::Conflict(
            "in-place mode needs an explicit user choice".into(),
        ));
    }
    let plan = UpdatePlan::new(&old, &new, allow_older)
        .map_err(|error| InstallError::Conflict(format!("invalid update: {error}")))?;
    control.set_phase(Phase::Allocating);
    let prepared = {
        let root = root.to_owned();
        let old = Arc::clone(&old);
        let new = Arc::clone(&new);
        let plan = plan.clone();
        let control = control.clone();
        tokio::task::spawn_blocking(move || prepare(&root, &old, &new, &plan, &control))
            .await
            .map_err(|error| InstallError::Internal(format!("in-place prepare: {error}")))??
    };
    let report = download::run(
        DownloadSpec {
            table: prepared.table,
            targets: prepared.targets,
            journal: prepared.journal,
            wanted: Some(prepared.wanted),
        },
        api,
        options,
        control,
    )
    .await?;
    let outcome = match report.outcome {
        RunOutcome::Completed => {
            control.set_phase(Phase::Finalizing);
            let root = root.to_owned();
            let record = tokio::task::spawn_blocking(move || {
                let record = commit::begin_inplace(&root, &old, &new)?;
                if let Err(error) = SafeRoot::open(&root).and_then(|safe| safe.remove_file(JOURNAL))
                {
                    tracing::warn!(%error, "cannot remove completed in-place journal");
                }
                Ok::<_, InstallError>(record)
            })
            .await
            .map_err(|error| InstallError::Internal(format!("in-place commit: {error}")))??;
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
    targets: Arc<Vec<Option<Target>>>,
    journal: Journal,
    wanted: Bitset,
}

fn key(new: &Release, table: &ChunkTable) -> JournalKey {
    JournalKey {
        version_id: new.manifest().version_id,
        manifest_blake3: *new.verified.digest.as_bytes(),
        chunk_count: table.len(),
    }
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
            "finish the interrupted commit before downloading".into(),
        ));
    }
    let mut record = read_record(root)?
        .ok_or_else(|| InstallError::Conflict("missing installed release".into()))?;
    if record.version_id != old.manifest().version_id
        || record.server_id != old.manifest().server_id
        || record.package_id != old.manifest().package_id
        || record.manifest_blake3 != old.verified.digest.to_hex()
        || !matches!(
            record.state,
            InstallState::Installed | InstallState::Updating
        )
    {
        return Err(InstallError::Conflict(
            "installed release changed before update".into(),
        ));
    }
    let safe = Arc::new(SafeRoot::open(root)?);
    let old_bytes = safe
        .read(
            &format!("{META_DIR}/{MANIFEST_FILE}"),
            MAX_MANIFEST_BYTES as u64,
        )?
        .ok_or_else(|| InstallError::Conflict("missing installed manifest".into()))?;
    if old_bytes != old.manifest_bytes {
        return Err(InstallError::Conflict(
            "installed manifest changed before update".into(),
        ));
    }
    let table = Arc::new(ChunkTable::new(new.manifest())?);
    let mut journal = Journal::in_root(Arc::clone(&safe), JOURNAL, key(new, &table));
    let mut wanted = Bitset::new(table.len());
    for needed in &plan.needed_chunks {
        wanted.set(needed.new_chunk);
    }
    if record.state == InstallState::Installed {
        let available = sys::available_space(safe.root())
            .map_err(|error| InstallError::io("inspect", safe.root(), error))?;
        let required = plan.inplace_extra_bytes.saturating_add(SPACE_MARGIN);
        if available < required {
            return Err(InstallError::NotEnoughSpace {
                required,
                available,
            });
        }
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
        safe.atomic_write(NEXT_MANIFEST, &new.manifest_bytes)?;
        safe.atomic_write(NEXT_SIGNATURE, &new.envelope.to_bytes())?;
        record.state = InstallState::Updating;
        write_record_in(&safe, &record)?;
    } else {
        let signature = new.envelope.to_bytes();
        for (name, expected) in [
            (NEXT_MANIFEST, new.manifest_bytes.as_slice()),
            (NEXT_SIGNATURE, signature.as_slice()),
        ] {
            let saved = safe
                .read(name, MAX_MANIFEST_BYTES as u64)?
                .ok_or_else(|| InstallError::Conflict("saved update release is missing".into()))?;
            if saved != expected {
                return Err(InstallError::Conflict(
                    "a different update is already in progress".into(),
                ));
            }
        }
    }
    let mut reset = false;
    for &index in &plan.build_files {
        let file = new
            .manifest()
            .files
            .get(index as usize)
            .ok_or_else(|| InstallError::Conflict("invalid build file index".into()))?;
        match safe.open_existing(&file.path, Access::Read) {
            Ok(Some(handle)) if handle.metadata().is_ok_and(|meta| meta.len() == file.size) => {}
            Ok(None)
            | Ok(Some(_))
            | Err(SafePathError::NotADirectory(_) | SafePathError::NotAFile(_)) => reset = true,
            Err(error) => return Err(error.into()),
        }
    }
    if !reset {
        for needed in &plan.needed_chunks {
            if journal.done().get(needed.new_chunk)
                && execute::read_old_chunk(&safe, new.manifest(), &table, needed.new_chunk)?
                    .is_none()
            {
                reset = true;
                break;
            }
        }
    }
    if reset {
        if safe.remove_file(JOURNAL)? {
            safe.sync_dir(META_DIR)?;
        }
        journal = Journal::in_root(Arc::clone(&safe), JOURNAL, key(new, &table));
    }
    commit::clear_removed(&safe, &plan.remove_files)?;
    let mut done_files = vec![false; new.manifest().files.len()];
    for needed in &plan.needed_chunks {
        if journal.done().get(needed.new_chunk) {
            for extent in table.extents(needed.new_chunk) {
                if let Some(slot) = done_files.get_mut(extent.file as usize) {
                    *slot = true;
                }
            }
        }
    }
    let mut targets = vec![None; new.manifest().files.len()];
    for &index in &plan.build_files {
        if control.is_cancelled() {
            return Err(InstallError::Cancelled);
        }
        let file = new
            .manifest()
            .files
            .get(index as usize)
            .ok_or_else(|| InstallError::Conflict("invalid build file index".into()))?;
        // Deleted and created through the root's handle, never by path (INS-07).
        let path = safe.path_of(&file.path);
        if !done_files.get(index as usize).copied().unwrap_or(false) {
            safe.remove_file(&file.path)?;
        }
        let handle = safe.open_file(&file.path, Access::CreateWrite)?;
        if let Err(error) = sys::preallocate(&handle, file.size) {
            if sys::is_disk_full(&error) {
                return Err(InstallError::NotEnoughSpace {
                    required: file.size,
                    available: sys::available_space(safe.root()).unwrap_or(0),
                });
            }
            return Err(InstallError::io("allocate", &path, error));
        }
        if let Some(slot) = targets.get_mut(index as usize) {
            *slot = Some(Target::new(Arc::clone(&safe), file.path.clone()));
        }
    }
    journal
        .persist()
        .map_err(|error| InstallError::io("write", journal.path(), error))?;
    Ok(Prepared {
        table,
        targets: Arc::new(targets),
        journal,
        wanted,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::install::{self, InstallRecord};
    use crate::testkit::package::{FileSpec, Identity, TestPackage, write_tree};
    use crate::testkit::{MockApi, Rig};
    use std::fs;
    use uuid::Uuid;
    use vgames_pack::Compression;

    fn setup() -> (tempfile::TempDir, TestPackage, TestPackage) {
        let old_files = [
            FileSpec::random("keep.bin", 32, 1),
            FileSpec::random("change.bin", 1024, 2),
        ];
        let new_files = [
            old_files[0].clone(),
            FileSpec::random("change.bin", 1024, 3),
        ];
        let old = TestPackage::build(&old_files, &[], Compression::None);
        let new = TestPackage::build_with(
            &new_files,
            &[],
            Compression::None,
            vgames_pack::PACK_SIZE,
            &Identity {
                version_id: Uuid::now_v7(),
                sequence: 2,
                ..Identity::default()
            },
        );
        let dir = tempfile::tempdir().unwrap();
        write_tree(dir.path(), &old_files, &[]);
        fs::create_dir(dir.path().join(META_DIR)).unwrap();
        fs::write(dir.path().join(META_DIR).join(MANIFEST_FILE), &old.manifest).unwrap();
        fs::write(
            dir.path().join(META_DIR).join("manifest.sig"),
            old.envelope.to_bytes(),
        )
        .unwrap();
        install::write_record(
            dir.path(),
            &InstallRecord::for_manifest(&old.release().verified, InstallState::Installed),
        )
        .unwrap();
        (dir, old, new)
    }

    #[tokio::test]
    async fn explicit_choice_is_required_before_any_mutation() {
        let (dir, old, new) = setup();
        let rig = Rig::start(new.manifest.clone(), new.packs.clone()).await;
        let api = MockApi::new(&rig, new.packs.iter().map(|p| p.len() as u64).collect());
        assert!(
            update_in_place(
                dir.path(),
                Arc::new(old.release()),
                Arc::new(new.release()),
                api,
                &DownloadOptions::default(),
                &DownloadControl::new(None),
                false,
                false,
            )
            .await
            .is_err()
        );
        assert_eq!(
            install::read_record(dir.path()).unwrap().unwrap().state,
            InstallState::Installed
        );
        assert_eq!(
            fs::read(dir.path().join("change.bin")).unwrap(),
            old.files[1].bytes()
        );
    }

    #[tokio::test]
    async fn paused_inplace_update_stays_unplayable_then_resumes() {
        let (dir, old, new) = setup();
        let rig = Rig::start(new.manifest.clone(), new.packs.clone()).await;
        let api = MockApi::new(&rig, new.packs.iter().map(|p| p.len() as u64).collect());
        let control = DownloadControl::new(None);
        control.pause();
        let paused = update_in_place(
            dir.path(),
            Arc::new(old.release()),
            Arc::new(new.release()),
            Arc::clone(&api),
            &DownloadOptions::default(),
            &control,
            false,
            true,
        )
        .await
        .unwrap();
        assert!(matches!(paused.outcome, InstallOutcome::Paused(_)));
        assert_eq!(
            install::read_record(dir.path()).unwrap().unwrap().state,
            InstallState::Updating
        );
        assert_ne!(
            fs::read(dir.path().join("change.bin")).unwrap(),
            old.files[1].bytes()
        );
        assert!(
            execute::update_safe(
                dir.path(),
                Arc::new(old.release()),
                Arc::new(new.release()),
                Arc::clone(&api),
                &DownloadOptions::default(),
                &DownloadControl::new(None),
                false,
            )
            .await
            .is_err()
        );
        control.resume();
        let completed = update_in_place(
            dir.path(),
            Arc::new(old.release()),
            Arc::new(new.release()),
            api,
            &DownloadOptions::default(),
            &control,
            false,
            true,
        )
        .await
        .unwrap();
        assert!(matches!(completed.outcome, InstallOutcome::Installed(_)));
        assert_eq!(
            fs::read(dir.path().join("change.bin")).unwrap(),
            new.files[1].bytes()
        );
        assert_eq!(
            fs::read(dir.path().join("keep.bin")).unwrap(),
            old.files[0].bytes()
        );
        assert_eq!(
            install::read_record(dir.path()).unwrap().unwrap().state,
            InstallState::Installed
        );
    }

    #[tokio::test]
    async fn damaged_completed_chunk_is_downloaded_again_on_resume() {
        let (dir, old, new) = setup();
        let rig = Rig::start(new.manifest.clone(), new.packs.clone()).await;
        let api = MockApi::new(&rig, new.packs.iter().map(|p| p.len() as u64).collect());
        let control = DownloadControl::new(None);
        control.pause();
        update_in_place(
            dir.path(),
            Arc::new(old.release()),
            Arc::new(new.release()),
            Arc::clone(&api),
            &DownloadOptions::default(),
            &control,
            false,
            true,
        )
        .await
        .unwrap();
        let release = new.release();
        let table = ChunkTable::new(release.manifest()).unwrap();
        let mut journal = Journal::load_or_new(&dir.path().join(JOURNAL), key(&release, &table));
        for index in 0..table.len() {
            journal.mark(index);
        }
        journal.persist().unwrap();
        fs::write(dir.path().join("change.bin"), vec![7; 1024]).unwrap();
        control.resume();
        let report = update_in_place(
            dir.path(),
            Arc::new(old.release()),
            Arc::new(release),
            api,
            &DownloadOptions::default(),
            &control,
            false,
            true,
        )
        .await
        .unwrap();
        assert!(matches!(report.outcome, InstallOutcome::Installed(_)));
        assert_eq!(
            fs::read(dir.path().join("change.bin")).unwrap(),
            new.files[1].bytes()
        );
        assert!(report.stats.network_bytes > 0);
    }

    #[tokio::test]
    async fn inplace_update_can_turn_an_old_file_into_a_folder() {
        let old_file = FileSpec::random("slot", 16, 1);
        let new_file = FileSpec::random("slot/game.bin", 16, 2);
        let old = TestPackage::build(std::slice::from_ref(&old_file), &[], Compression::None);
        let new = TestPackage::build_with(
            std::slice::from_ref(&new_file),
            &[],
            Compression::None,
            vgames_pack::PACK_SIZE,
            &Identity {
                version_id: Uuid::now_v7(),
                sequence: 2,
                ..Identity::default()
            },
        );
        let dir = tempfile::tempdir().unwrap();
        write_tree(dir.path(), &[old_file], &[]);
        fs::create_dir(dir.path().join(META_DIR)).unwrap();
        fs::write(dir.path().join(META_DIR).join(MANIFEST_FILE), &old.manifest).unwrap();
        fs::write(
            dir.path().join(META_DIR).join("manifest.sig"),
            old.envelope.to_bytes(),
        )
        .unwrap();
        install::write_record(
            dir.path(),
            &InstallRecord::for_manifest(&old.release().verified, InstallState::Installed),
        )
        .unwrap();
        let rig = Rig::start(new.manifest.clone(), new.packs.clone()).await;
        let api = MockApi::new(&rig, new.packs.iter().map(|p| p.len() as u64).collect());
        let result = update_in_place(
            dir.path(),
            Arc::new(old.release()),
            Arc::new(new.release()),
            api,
            &DownloadOptions::default(),
            &DownloadControl::new(None),
            false,
            true,
        )
        .await
        .unwrap();
        assert!(matches!(result.outcome, InstallOutcome::Installed(_)));
        assert_eq!(
            fs::read(dir.path().join("slot/game.bin")).unwrap(),
            new_file.bytes()
        );
        let final_file = FileSpec::random("slot", 16, 3);
        let final_package = TestPackage::build_with(
            std::slice::from_ref(&final_file),
            &[],
            Compression::None,
            vgames_pack::PACK_SIZE,
            &Identity {
                version_id: Uuid::now_v7(),
                sequence: 3,
                ..Identity::default()
            },
        );
        let rig = Rig::start(final_package.manifest.clone(), final_package.packs.clone()).await;
        let api = MockApi::new(
            &rig,
            final_package.packs.iter().map(|p| p.len() as u64).collect(),
        );
        let result = update_in_place(
            dir.path(),
            Arc::new(new.release()),
            Arc::new(final_package.release()),
            api,
            &DownloadOptions::default(),
            &DownloadControl::new(None),
            false,
            true,
        )
        .await
        .unwrap();
        assert!(matches!(result.outcome, InstallOutcome::Installed(_)));
        assert_eq!(
            fs::read(dir.path().join("slot")).unwrap(),
            final_file.bytes()
        );
    }
}
