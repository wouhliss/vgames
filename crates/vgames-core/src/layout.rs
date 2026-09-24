//! Deterministic chunk and pack layout (02-package-format §4 steps 3 and 5).
//!
//! The **single** implementation: the packer (`vgames-pack`) lays packages out
//! with it, and the manifest validator recomputes the layout from file sizes
//! and requires the manifest to match it exactly.
//!
//! Chunk assignment, iterating files in manifest order:
//! - **Empty file:** no chunk.
//! - **Large file** (`size ≥ chunk_size`): close the open shared chunk; the file
//!   gets `ceil(size / chunk_size)` exclusive chunks, all full except the last.
//! - **Small file:** if the open shared chunk cannot fit it, close that chunk;
//!   append the file to the open shared chunk at `offset = bytes already in it`.
//!
//! Packs: append stored chunk bytes to the current pack; start a new pack when
//! the next chunk would push it over `pack_size`.

/// Fixed chunk size of `vgames.manifest/1` (4 MiB).
pub const CHUNK_SIZE: u64 = 4 * 1024 * 1024;
/// Default and maximum pack size (256 MiB).
pub const PACK_SIZE: u64 = 256 * 1024 * 1024;
/// Worst-case zstd expansion allowed for a stored chunk (`stored_size ≤ size + 64 KiB`).
pub const MAX_STORED_OVERHEAD: u64 = 64 * 1024;

/// Where a file's bytes live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSlot {
    /// First chunk holding the file; `None` for an empty file.
    pub chunk: Option<u32>,
    /// Byte offset inside that chunk (always 0 for large files).
    pub offset: u64,
}

/// One chunk: its decoded size and the files (by index) it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSlot {
    pub size: u64,
    /// Index of the first file whose bytes are in this chunk.
    pub first_file: u32,
    /// Number of consecutive files from `first_file` spanned by this chunk
    /// (empty files in the span hold no bytes).
    pub file_count: u32,
    /// True when the chunk belongs to one large file (`size ≥ chunk_size`).
    pub exclusive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub chunk_size: u64,
    pub files: Vec<FileSlot>,
    pub chunks: Vec<ChunkSlot>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    #[error("chunk size must be positive")]
    ZeroChunkSize,
    #[error("too many chunks (more than 2^32)")]
    TooManyChunks,
    #[error("too many files (more than 2^32)")]
    TooManyFiles,
    #[error("chunk {chunk} is {size} bytes, larger than the pack size")]
    ChunkLargerThanPack { chunk: usize, size: u64 },
    #[error("chunk {chunk} stores zero bytes")]
    EmptyChunk { chunk: usize },
    #[error("total size overflows")]
    Overflow,
}

/// Assigns chunks to files given in manifest order (02 §4 step 3).
///
/// Allocates one entry per chunk: callers holding **untrusted** sizes must use
/// [`assign_chunks_bounded`].
pub fn assign_chunks(file_sizes: &[u64], chunk_size: u64) -> Result<Layout, LayoutError> {
    assign_chunks_bounded(file_sizes, chunk_size, u64::from(u32::MAX))
}

/// Number of chunks [`assign_chunks`] would produce, without allocating.
pub fn count_chunks(file_sizes: &[u64], chunk_size: u64) -> Result<u64, LayoutError> {
    if chunk_size == 0 {
        return Err(LayoutError::ZeroChunkSize);
    }
    let mut count = 0u64;
    // Bytes in the open shared chunk; 0 means no open chunk.
    let mut open = 0u64;
    for &size in file_sizes {
        if size == 0 {
            continue;
        }
        if size >= chunk_size {
            open = 0;
            count = count
                .checked_add(size.div_ceil(chunk_size))
                .ok_or(LayoutError::Overflow)?;
        } else if open == 0 || open + size > chunk_size {
            count = count.checked_add(1).ok_or(LayoutError::Overflow)?;
            open = size;
        } else {
            open += size;
        }
    }
    Ok(count)
}

/// As [`assign_chunks`], but fails with [`LayoutError::TooManyChunks`] before
/// allocating anything if the layout would exceed `max_chunks` chunks.
pub fn assign_chunks_bounded(
    file_sizes: &[u64],
    chunk_size: u64,
    max_chunks: u64,
) -> Result<Layout, LayoutError> {
    let total = count_chunks(file_sizes, chunk_size)?;
    if total > max_chunks || total > u64::from(u32::MAX) {
        return Err(LayoutError::TooManyChunks);
    }
    if u32::try_from(file_sizes.len()).is_err() {
        return Err(LayoutError::TooManyFiles);
    }
    let mut files = Vec::with_capacity(file_sizes.len());
    let mut chunks: Vec<ChunkSlot> = Vec::with_capacity(usize::try_from(total).unwrap_or(0));
    // Index of the open shared chunk, if any.
    let mut open: Option<u32> = None;

    for (file, &size) in (0u32..).zip(file_sizes) {
        if size == 0 {
            files.push(FileSlot {
                chunk: None,
                offset: 0,
            });
            continue;
        }
        // `total ≤ u32::MAX` was checked above, so indices fit.
        let next = u32::try_from(chunks.len()).map_err(|_| LayoutError::TooManyChunks)?;
        if size >= chunk_size {
            open = None;
            let count = size.div_ceil(chunk_size);
            for k in 0..count {
                let this = if k + 1 == count {
                    size - k * chunk_size
                } else {
                    chunk_size
                };
                chunks.push(ChunkSlot {
                    size: this,
                    first_file: file,
                    file_count: 1,
                    exclusive: true,
                });
            }
            files.push(FileSlot {
                chunk: Some(next),
                offset: 0,
            });
            continue;
        }
        // Small file: append to the open shared chunk if it fits, else open a new one.
        let target = match open.and_then(|c| chunks.get_mut(c as usize)) {
            Some(slot) if slot.size + size <= chunk_size => slot,
            _ => {
                chunks.push(ChunkSlot {
                    size: 0,
                    first_file: file,
                    file_count: 0,
                    exclusive: false,
                });
                open = Some(next);
                chunks.last_mut().ok_or(LayoutError::TooManyChunks)?
            }
        };
        files.push(FileSlot {
            chunk: Some(open.unwrap_or(next)),
            offset: target.size,
        });
        target.size += size;
        // Spans from the first to this file; empty files in between hold no bytes.
        target.file_count = file - target.first_file + 1;
    }
    Ok(Layout {
        chunk_size,
        files,
        chunks,
    })
}

/// A pack: consecutive chunks whose stored bytes are concatenated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackSlot {
    pub first_chunk: u32,
    pub chunk_count: u32,
    /// Σ stored sizes.
    pub size: u64,
}

/// Where each chunk's stored bytes sit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkPlacement {
    pub pack: u32,
    pub offset: u64,
}

/// Lays chunks into packs in order (02 §4 step 5): a new pack starts when the
/// next chunk would push the current one over `pack_size`. Takes each chunk's
/// **stored** size.
pub fn packs(
    stored_sizes: &[u64],
    pack_size: u64,
) -> Result<(Vec<PackSlot>, Vec<ChunkPlacement>), LayoutError> {
    assign_packs(stored_sizes, pack_size)
}

/// Same as [`packs`] (the name used by `vgames-pack`'s interim module).
pub fn assign_packs(
    stored_sizes: &[u64],
    pack_size: u64,
) -> Result<(Vec<PackSlot>, Vec<ChunkPlacement>), LayoutError> {
    let mut packs: Vec<PackSlot> = Vec::new();
    let mut placements = Vec::with_capacity(stored_sizes.len());
    for (i, &size) in stored_sizes.iter().enumerate() {
        if size > pack_size {
            return Err(LayoutError::ChunkLargerThanPack { chunk: i, size });
        }
        if size == 0 {
            return Err(LayoutError::EmptyChunk { chunk: i });
        }
        let chunk = u32::try_from(i).map_err(|_| LayoutError::TooManyChunks)?;
        let start_new = packs
            .last()
            .is_none_or(|p| p.size.saturating_add(size) > pack_size);
        if start_new {
            packs.push(PackSlot {
                first_chunk: chunk,
                chunk_count: 0,
                size: 0,
            });
        }
        let pack_index = u32::try_from(packs.len() - 1).map_err(|_| LayoutError::TooManyChunks)?;
        let Some(pack) = packs.last_mut() else {
            continue;
        };
        placements.push(ChunkPlacement {
            pack: pack_index,
            offset: pack.size,
        });
        pack.size += size;
        pack.chunk_count += 1;
    }
    Ok((packs, placements))
}

impl Layout {
    /// The byte extents that make up chunk `chunk`, in order:
    /// `(file index, offset in file, length)`.
    pub fn extents(&self, sizes: &[u64], chunk: u32) -> Vec<(u32, u64, u64)> {
        let Some(slot) = self.chunks.get(chunk as usize) else {
            return Vec::new();
        };
        if slot.exclusive {
            let Some(first) = self
                .files
                .get(slot.first_file as usize)
                .and_then(|f| f.chunk)
            else {
                return Vec::new();
            };
            let k = u64::from(chunk - first);
            return vec![(slot.first_file, k * self.chunk_size, slot.size)];
        }
        (slot.first_file..slot.first_file + slot.file_count)
            .filter_map(|f| {
                let size = *sizes.get(f as usize)?;
                (size > 0).then_some((f, 0, size))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const C: u64 = 16;

    #[test]
    fn empty_files_get_no_chunk() {
        let l = assign_chunks(&[0, 0], C).unwrap();
        assert!(l.chunks.is_empty());
        assert!(l.files.iter().all(|f| f.chunk.is_none()));
    }

    #[test]
    fn small_files_share_and_large_files_close_the_shared_chunk() {
        // 5, 6 share chunk 0; 20 closes it (chunks 1, 2); 3 opens chunk 3; 10 does not fit → chunk 4.
        let l = assign_chunks(&[5, 6, 20, 3, 14], C).unwrap();
        let slots: Vec<_> = l.files.iter().map(|f| (f.chunk, f.offset)).collect();
        assert_eq!(
            slots,
            vec![
                (Some(0), 0),
                (Some(0), 5),
                (Some(1), 0),
                (Some(3), 0),
                (Some(4), 0)
            ]
        );
        let sizes: Vec<_> = l.chunks.iter().map(|c| c.size).collect();
        assert_eq!(sizes, vec![11, 16, 4, 3, 14]);
    }

    #[test]
    fn exact_chunk_size_file_is_large() {
        let l = assign_chunks(&[1, C, 1], C).unwrap();
        let slots: Vec<_> = l.files.iter().map(|f| f.chunk).collect();
        assert_eq!(slots, vec![Some(0), Some(1), Some(2)]);
        assert!(l.chunks[1].exclusive);
    }

    #[test]
    fn huge_untrusted_sizes_fail_before_allocating() {
        assert_eq!(
            count_chunks(&[u64::MAX], CHUNK_SIZE).unwrap(),
            u64::MAX.div_ceil(CHUNK_SIZE)
        );
        assert_eq!(
            assign_chunks_bounded(&[u64::MAX], CHUNK_SIZE, 1000),
            Err(LayoutError::TooManyChunks)
        );
        assert_eq!(
            assign_chunks(&[u64::MAX, u64::MAX], 1),
            Err(LayoutError::Overflow)
        );
        assert_eq!(assign_chunks(&[1], 0), Err(LayoutError::ZeroChunkSize));
    }

    #[test]
    fn packs_split_before_overflow() {
        let (packs, placements) = assign_packs(&[10, 10, 5, 7], 25).unwrap();
        assert_eq!(packs.len(), 2);
        assert_eq!(packs[0].size, 25);
        assert_eq!(packs[1].size, 7);
        assert_eq!(placements[3], ChunkPlacement { pack: 1, offset: 0 });
        assert!(assign_packs(&[26], 25).is_err());
        assert_eq!(
            super::packs(&[3, 0], 25),
            Err(LayoutError::EmptyChunk { chunk: 1 })
        );
    }

    proptest! {
        /// Every file byte is covered exactly once, in order, and extents rebuild each chunk's size.
        #[test]
        fn layout_covers_every_byte_once(sizes in prop::collection::vec(prop_oneof![Just(0u64), 1u64..C, Just(C), C..5 * C], 0..60)) {
            let l = assign_chunks(&sizes, C).unwrap();
            prop_assert_eq!(l.files.len(), sizes.len());
            prop_assert_eq!(count_chunks(&sizes, C).unwrap(), l.chunks.len() as u64);
            let mut covered = vec![0u64; sizes.len()];
            for (i, chunk) in l.chunks.iter().enumerate() {
                prop_assert!(chunk.size > 0 && chunk.size <= C);
                let extents = l.extents(&sizes, i as u32);
                let total: u64 = extents.iter().map(|e| e.2).sum();
                prop_assert_eq!(total, chunk.size);
                for (f, off, len) in extents {
                    prop_assert_eq!(off, covered[f as usize]);
                    covered[f as usize] += len;
                }
            }
            prop_assert_eq!(covered, sizes.clone());
            // Large files: whole chunks, all full except the last.
            for (f, &size) in sizes.iter().enumerate() {
                if size >= C {
                    let first = l.files[f].chunk.unwrap() as usize;
                    let n = size.div_ceil(C) as usize;
                    for k in 0..n - 1 {
                        prop_assert_eq!(l.chunks[first + k].size, C);
                    }
                }
            }
        }

        #[test]
        fn packs_are_contiguous_and_bounded(sizes in prop::collection::vec(1u64..30, 0..50)) {
            let (packs, placements) = assign_packs(&sizes, 40).unwrap();
            let mut next = 0u32;
            for (p, pack) in packs.iter().enumerate() {
                prop_assert_eq!(pack.first_chunk, next);
                prop_assert!(pack.size <= 40);
                let mut offset = 0;
                for c in pack.first_chunk..pack.first_chunk + pack.chunk_count {
                    prop_assert_eq!(placements[c as usize], ChunkPlacement { pack: p as u32, offset });
                    offset += sizes[c as usize];
                }
                prop_assert_eq!(offset, pack.size);
                next += pack.chunk_count;
            }
            prop_assert_eq!(next as usize, sizes.len());
        }
    }
}
