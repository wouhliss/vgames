//! The packing plan: validated, sorted files with their chunk layout, and the
//! placement of chunks into packs (02-package-format §4).

use serde::{Deserialize, Serialize};

use crate::encode::Encoding;
use crate::layout::{self, CHUNK_SIZE, ChunkPlacement, Layout, LayoutError, PackSlot};
use crate::paths::{self, PathError};

/// One source file, as found by a scan (native) or reported by the browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFile {
    /// Manifest path (`/`-separated, relative to the package root).
    pub path: String,
    pub size: u64,
    /// Modification time in Unix milliseconds. The upload aborts if it changes.
    pub mtime_ms: i64,
    #[serde(default)]
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("{} invalid path(s), first: {}", .0.len(), .0.first().map(ToString::to_string).unwrap_or_default())]
    InvalidPaths(Vec<PathError>),
    #[error("cannot lay out chunks")]
    Layout(#[from] LayoutError),
    #[error("pack size must be between one chunk and 256 MiB")]
    PackSize,
    #[error("the packing does not match the plan's chunk count")]
    PackingMismatch,
}

/// A validated, deterministic plan. Same tree → same plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    files: Vec<SourceFile>,
    directories: Vec<String>,
    layout: Layout,
    sizes: Vec<u64>,
}

impl Plan {
    /// Sorts files by path (byte-wise UTF-8), validates every path and lays out
    /// chunks. `directories` are empty folders to recreate on install.
    pub fn new(
        mut files: Vec<SourceFile>,
        mut directories: Vec<String>,
    ) -> Result<Self, PlanError> {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        directories.sort();
        directories.dedup();

        let mut invalid: Vec<PathError> = files
            .iter()
            .map(|f| f.path.as_str())
            .chain(directories.iter().map(String::as_str))
            .filter_map(|p| paths::validate_path(p).err())
            .collect();
        if invalid.is_empty()
            && let Err(e) = paths::validate_tree(
                files.iter().map(|f| f.path.as_str()),
                directories.iter().map(String::as_str),
            )
        {
            invalid.push(e);
        }
        if !invalid.is_empty() {
            return Err(PlanError::InvalidPaths(invalid));
        }

        let sizes: Vec<u64> = files.iter().map(|f| f.size).collect();
        let layout = layout::assign_chunks(&sizes, CHUNK_SIZE)?;
        Ok(Self {
            files,
            directories,
            layout,
            sizes,
        })
    }

    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }

    pub fn directories(&self) -> &[String] {
        &self.directories
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn chunk_size(&self) -> u64 {
        self.layout.chunk_size
    }

    pub fn chunk_count(&self) -> u32 {
        // assign_chunks guarantees the count fits in u32.
        u32::try_from(self.layout.chunks.len()).unwrap_or(u32::MAX)
    }

    pub fn total_bytes(&self) -> u64 {
        self.sizes.iter().sum()
    }

    /// Decoded size of a chunk.
    pub fn chunk_len(&self, chunk: u32) -> u64 {
        self.layout.chunks.get(chunk as usize).map_or(0, |c| c.size)
    }

    /// `(file index, offset in file, length)` extents of a chunk, in order.
    pub fn extents(&self, chunk: u32) -> Vec<(u32, u64, u64)> {
        self.layout.extents(&self.sizes, chunk)
    }

    /// Packing with every chunk raw: known without reading any byte.
    pub fn raw_packing(&self, pack_size: u64) -> Result<Packing, PlanError> {
        let chunks: Vec<(Encoding, u64)> = self
            .layout
            .chunks
            .iter()
            .map(|c| (Encoding::Raw, c.size))
            .collect();
        Packing::new(self, &chunks, pack_size)
    }
}

/// How chunks are stored in packs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packing {
    pub chunks: Vec<PackedChunk>,
    pub packs: Vec<PackSlot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedChunk {
    pub encoding: Encoding,
    pub stored_size: u64,
    pub placement: ChunkPlacement,
}

impl Packing {
    /// Lays out chunks given each one's `(encoding, stored size)`.
    pub fn new(plan: &Plan, chunks: &[(Encoding, u64)], pack_size: u64) -> Result<Self, PlanError> {
        if !(CHUNK_SIZE + crate::encode::MAX_STORED_OVERHEAD..=layout::PACK_SIZE)
            .contains(&pack_size)
        {
            return Err(PlanError::PackSize);
        }
        if chunks.len() != plan.layout.chunks.len() {
            return Err(PlanError::PackingMismatch);
        }
        let stored: Vec<u64> = chunks.iter().map(|c| c.1).collect();
        let (packs, placements) = layout::assign_packs(&stored, pack_size)?;
        Ok(Self {
            chunks: chunks
                .iter()
                .zip(placements)
                .map(|(&(encoding, stored_size), placement)| PackedChunk {
                    encoding,
                    stored_size,
                    placement,
                })
                .collect(),
            packs,
        })
    }

    pub fn pack_count(&self) -> u32 {
        u32::try_from(self.packs.len()).unwrap_or(u32::MAX)
    }

    pub fn total_stored(&self) -> u64 {
        self.packs.iter().map(|p| p.size).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> SourceFile {
        SourceFile {
            path: path.into(),
            size,
            mtime_ms: 0,
            executable: false,
        }
    }

    #[test]
    fn sorts_bytewise_and_is_deterministic() {
        let a = Plan::new(
            vec![file("b", 1), file("a/z", 2), file("Z", 3), file("é", 4)],
            vec![],
        )
        .unwrap();
        let b = Plan::new(
            vec![file("é", 4), file("Z", 3), file("b", 1), file("a/z", 2)],
            vec![],
        )
        .unwrap();
        assert_eq!(a, b);
        let order: Vec<_> = a.files().iter().map(|f| f.path.as_str()).collect();
        assert_eq!(order, ["Z", "a/z", "b", "é"]);
    }

    #[test]
    fn lists_every_invalid_path() {
        let err =
            Plan::new(vec![file("ok", 1), file("CON", 1), file("a:b", 1)], vec![]).unwrap_err();
        match err {
            PlanError::InvalidPaths(list) => assert_eq!(list.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn raw_packing_splits_at_pack_size() {
        let files: Vec<_> = (0..70)
            .map(|i| file(&format!("f{i:03}"), CHUNK_SIZE))
            .collect();
        let plan = Plan::new(files, vec![]).unwrap();
        let packing = plan.raw_packing(layout::PACK_SIZE).unwrap();
        assert_eq!(packing.packs.len(), 2);
        assert_eq!(packing.packs[0].chunk_count, 64);
        assert_eq!(packing.total_stored(), plan.total_bytes());
        assert!(plan.raw_packing(1024).is_err());
    }
}
