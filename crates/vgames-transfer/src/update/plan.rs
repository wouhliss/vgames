//! Pure update planner. It never opens a file: the caller must verify a local
//! reuse candidate's bytes against the signed old chunk before copying them.

use std::collections::{BTreeMap, HashMap};

use vgames_core::manifest::Manifest;

use crate::download::table::{ChunkTable, TableError};
use crate::install::Release;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Safe,
    InPlace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Local { old_chunk: u32 },
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeededChunk {
    pub new_chunk: u32,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeptFile {
    pub old_index: u32,
    pub new_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePlan {
    pub kept_files: Vec<KeptFile>,
    /// Indices into the new signed manifest.
    pub build_files: Vec<u32>,
    /// Paths to remove only at commit time in safe mode.
    pub remove_files: Vec<String>,
    pub needed_chunks: Vec<NeededChunk>,
    /// Additional file bytes required for safe staging. This excludes the
    /// journal and the required 64 MiB free-space margin.
    pub safe_extra_bytes: u64,
    /// A conservative lower bound after deleting replaced files first.
    pub inplace_extra_bytes: u64,
    pub remote_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("the new release belongs to another package or platform")]
    Identity,
    #[error("the new sequence is not newer; explicitly choose an older version to proceed")]
    Rollback,
    #[error("the signed manifest layout is inconsistent: {0}")]
    Layout(#[from] TableError),
    #[error("the update size overflows the supported range")]
    Size,
}

impl UpdatePlan {
    /// Both releases must have passed `verify_manifest` under current trust.
    /// `allow_older` represents an explicit user choice or confirmed yank.
    pub fn new(old: &Release, new: &Release, allow_older: bool) -> Result<Self, PlanError> {
        let old = old.manifest();
        let new = new.manifest();
        if old.server_id != new.server_id
            || old.package_id != new.package_id
            || old.platform != new.platform
        {
            return Err(PlanError::Identity);
        }
        if new.sequence <= old.sequence && !allow_older {
            return Err(PlanError::Rollback);
        }
        Self::from_manifests(old, new)
    }

    fn from_manifests(old: &Manifest, new: &Manifest) -> Result<Self, PlanError> {
        let old_paths: BTreeMap<&str, (u32, &vgames_core::manifest::File)> = old
            .files
            .iter()
            .enumerate()
            .map(|(i, file)| {
                u32::try_from(i)
                    .map(|index| (file.path.as_str(), (index, file)))
                    .map_err(|_| PlanError::Size)
            })
            .collect::<Result<_, _>>()?;
        let new_paths: BTreeMap<&str, ()> =
            new.files.iter().map(|f| (f.path.as_str(), ())).collect();
        let mut kept_files = Vec::new();
        let mut build_files = Vec::new();
        let mut safe_extra_bytes = 0u64;
        let mut replaced_old_bytes = 0u64;
        for (new_index, file) in new.files.iter().enumerate() {
            let new_index = u32::try_from(new_index).map_err(|_| PlanError::Size)?;
            let old_match = old_paths.get(file.path.as_str()).copied();
            if let Some((old_index, previous)) = old_match
                && previous.size == file.size
                && previous.blake3 == file.blake3
                && previous.executable == file.executable
            {
                kept_files.push(KeptFile {
                    old_index,
                    new_index,
                });
            } else {
                build_files.push(new_index);
                safe_extra_bytes = safe_extra_bytes
                    .checked_add(file.size)
                    .ok_or(PlanError::Size)?;
                if let Some((_, previous)) = old_match {
                    replaced_old_bytes = replaced_old_bytes
                        .checked_add(previous.size)
                        .ok_or(PlanError::Size)?;
                }
            }
        }
        let remove_files = old
            .files
            .iter()
            .filter(|f| !new_paths.contains_key(f.path.as_str()))
            .map(|f| f.path.clone())
            .collect();

        let mut old_hashes: HashMap<([u8; 32], u64), u32> = HashMap::new();
        for (index, chunk) in old.chunks.iter().enumerate() {
            old_hashes
                .entry((*chunk.blake3.as_bytes(), chunk.size))
                .or_insert(index as u32);
        }
        let table = ChunkTable::new(new)?;
        let mut needed_chunks = Vec::new();
        let mut remote_bytes = 0u64;
        let mut build_flags = vec![false; new.files.len()];
        for &file in &build_files {
            *build_flags.get_mut(file as usize).ok_or(PlanError::Size)? = true;
        }
        for (index, chunk) in new.chunks.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| PlanError::Size)?;
            if table.extents(index).iter().any(|extent| {
                build_flags
                    .get(extent.file as usize)
                    .copied()
                    .unwrap_or(false)
            }) {
                let source = match old_hashes.get(&(*chunk.blake3.as_bytes(), chunk.size)) {
                    Some(&old_chunk) => Source::Local { old_chunk },
                    None => {
                        remote_bytes = remote_bytes
                            .checked_add(chunk.stored_size)
                            .ok_or(PlanError::Size)?;
                        Source::Remote
                    }
                };
                needed_chunks.push(NeededChunk {
                    new_chunk: index,
                    source,
                });
            }
        }
        Ok(Self {
            kept_files,
            build_files,
            remove_files,
            needed_chunks,
            safe_extra_bytes,
            inplace_extra_bytes: safe_extra_bytes.saturating_sub(replaced_old_bytes),
            remote_bytes,
        })
    }

    /// Choose the default safe mode when space permits; in-place needs an
    /// explicit user choice because it makes the package unplayable mid-update.
    pub fn choose_mode(&self, available: u64, allow_inplace: bool) -> Option<Mode> {
        let margin = crate::install::SPACE_MARGIN;
        if self
            .safe_extra_bytes
            .checked_add(margin)
            .is_some_and(|required| available >= required)
        {
            Some(Mode::Safe)
        } else if allow_inplace
            && self
                .inplace_extra_bytes
                .checked_add(margin)
                .is_some_and(|required| available >= required)
        {
            Some(Mode::InPlace)
        } else {
            None
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::testkit::package::{FileSpec, Identity, TestPackage};
    use uuid::Uuid;
    use vgames_pack::Compression;

    fn pair(old_files: &[FileSpec], new_files: &[FileSpec]) -> (TestPackage, TestPackage) {
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

    #[test]
    fn unchanged_files_stay_and_only_changed_chunks_are_fetched() {
        let files = (0..10_000)
            .map(|i| FileSpec::random(&format!("file-{i:03}.bin"), 100, i))
            .collect::<Vec<_>>();
        let mut changed = files.clone();
        changed[4_999] = FileSpec::random("file-4999.bin", 100, 999);
        let (old, new) = pair(&files, &changed);
        let plan = UpdatePlan::new(&old.release(), &new.release(), false).unwrap();
        assert_eq!(plan.kept_files.len(), 9_999);
        assert_eq!(plan.build_files.len(), 1);
        assert_eq!(plan.needed_chunks.len(), 1); // 10,000 tiny files share a chunk.
        assert_eq!(plan.remote_bytes, 1_000_000);
        assert_eq!(
            plan.choose_mode(crate::install::SPACE_MARGIN + 100, false),
            Some(Mode::Safe)
        );
        assert_eq!(
            plan.choose_mode(crate::install::SPACE_MARGIN, true),
            Some(Mode::InPlace)
        );
    }

    #[test]
    fn identical_content_at_a_new_path_reuses_a_local_chunk() {
        let (old, new) = pair(
            &[FileSpec::random("old.bin", 4 * 1024 * 1024, 3)],
            &[FileSpec::random("new.bin", 4 * 1024 * 1024, 3)],
        );
        let plan = UpdatePlan::new(&old.release(), &new.release(), false).unwrap();
        assert_eq!(plan.remove_files, ["old.bin"]);
        assert_eq!(
            plan.needed_chunks,
            [NeededChunk {
                new_chunk: 0,
                source: Source::Local { old_chunk: 0 }
            }]
        );
        assert_eq!(plan.remote_bytes, 0);
    }

    #[test]
    fn rollback_needs_an_explicit_choice() {
        let (old, new) = pair(
            &[FileSpec::random("x", 1, 1)],
            &[FileSpec::random("x", 1, 1)],
        );
        assert!(matches!(
            UpdatePlan::new(&new.release(), &old.release(), false),
            Err(PlanError::Rollback)
        ));
        assert!(UpdatePlan::new(&new.release(), &old.release(), true).is_ok());
    }
}
