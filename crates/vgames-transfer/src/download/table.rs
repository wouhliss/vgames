//! What the engine needs from a verified manifest: every chunk's place in its
//! pack, and the file extents its decoded bytes go to (02 §4: extents are
//! implied by `(chunk, offset, size)`).

use vgames_core::manifest::{Encoding, Manifest};

/// One chunk of the manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkInfo {
    pub pack: u32,
    /// Offset of the stored bytes in the pack.
    pub offset: u64,
    pub stored_size: u64,
    /// Decoded size.
    pub size: u64,
    pub encoding: Encoding,
    pub blake3: [u8; 32],
}

/// A run of a chunk's decoded bytes that belongs to one file. A chunk's
/// extents are in buffer order and cover it exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    pub file: u32,
    pub file_offset: u64,
    pub len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the manifest layout is inconsistent at chunk {chunk}")]
pub struct TableError {
    pub chunk: u32,
}

#[derive(Debug, Clone)]
pub struct ChunkTable {
    chunks: Vec<ChunkInfo>,
    /// `extents[extent_start[c]..extent_start[c + 1]]` belong to chunk `c`.
    extent_start: Vec<u32>,
    extents: Vec<Extent>,
    pack_sizes: Vec<u64>,
    chunk_size: u64,
}

impl ChunkTable {
    /// Builds the table from a manifest that passed `verify_manifest`
    /// (which already checked the layout); inconsistencies are still refused.
    pub fn new(manifest: &Manifest) -> Result<Self, TableError> {
        let chunk_size = manifest.chunk_size;
        let chunks: Vec<ChunkInfo> = manifest
            .chunks
            .iter()
            .map(|c| ChunkInfo {
                pack: c.pack,
                offset: c.offset,
                stored_size: c.stored_size,
                size: c.size,
                encoding: c.encoding,
                blake3: *c.blake3.as_bytes(),
            })
            .collect();
        let n = chunks.len();
        let mut per_chunk: Vec<Vec<Extent>> = vec![Vec::new(); n];
        let mut filled = vec![0u64; n];
        for (index, file) in manifest.files.iter().enumerate() {
            let Some(first) = file.chunk else { continue };
            let file_index = u32::try_from(index).map_err(|_| TableError { chunk: first })?;
            if file.size >= chunk_size {
                let count = file.size.div_ceil(chunk_size);
                for k in 0..count {
                    let chunk = first
                        .checked_add(u32::try_from(k).map_err(|_| TableError { chunk: first })?)
                        .ok_or(TableError { chunk: first })?;
                    let len = (file.size - k * chunk_size).min(chunk_size);
                    let slot = per_chunk
                        .get_mut(chunk as usize)
                        .ok_or(TableError { chunk })?;
                    let used = filled.get_mut(chunk as usize).ok_or(TableError { chunk })?;
                    if *used != 0 {
                        return Err(TableError { chunk });
                    }
                    slot.push(Extent {
                        file: file_index,
                        file_offset: k * chunk_size,
                        len,
                    });
                    *used = len;
                }
            } else {
                let slot = per_chunk
                    .get_mut(first as usize)
                    .ok_or(TableError { chunk: first })?;
                let used = filled
                    .get_mut(first as usize)
                    .ok_or(TableError { chunk: first })?;
                if *used != file.offset {
                    return Err(TableError { chunk: first });
                }
                slot.push(Extent {
                    file: file_index,
                    file_offset: 0,
                    len: file.size,
                });
                *used += file.size;
            }
        }
        for (i, (chunk, used)) in chunks.iter().zip(&filled).enumerate() {
            if chunk.size != *used {
                return Err(TableError {
                    chunk: u32::try_from(i).unwrap_or(u32::MAX),
                });
            }
        }
        let mut extent_start = Vec::with_capacity(n + 1);
        let mut extents = Vec::new();
        for list in per_chunk {
            extent_start.push(u32::try_from(extents.len()).map_err(|_| TableError { chunk: 0 })?);
            extents.extend(list);
        }
        extent_start.push(u32::try_from(extents.len()).map_err(|_| TableError { chunk: 0 })?);
        Ok(Self {
            chunks,
            extent_start,
            extents,
            pack_sizes: manifest.packs.iter().map(|p| p.size).collect(),
            chunk_size,
        })
    }

    pub fn len(&self) -> u32 {
        u32::try_from(self.chunks.len()).unwrap_or(u32::MAX)
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    pub fn chunk(&self, index: u32) -> Option<&ChunkInfo> {
        self.chunks.get(index as usize)
    }

    pub fn chunks(&self) -> &[ChunkInfo] {
        &self.chunks
    }

    pub fn extents(&self, index: u32) -> &[Extent] {
        let i = index as usize;
        let (Some(&start), Some(&end)) = (self.extent_start.get(i), self.extent_start.get(i + 1))
        else {
            return &[];
        };
        self.extents
            .get(start as usize..end as usize)
            .unwrap_or(&[])
    }

    pub fn pack_size(&self, pack: u32) -> Option<u64> {
        self.pack_sizes.get(pack as usize).copied()
    }

    pub fn pack_count(&self) -> u32 {
        u32::try_from(self.pack_sizes.len()).unwrap_or(u32::MAX)
    }

    pub fn chunk_size(&self) -> u64 {
        self.chunk_size
    }

    /// Byte range `[start, end)` in the pack covering `count` chunks from `first`.
    pub fn byte_range(&self, first: u32, count: u32) -> Option<(u64, u64)> {
        let a = self.chunk(first)?;
        let b = self.chunk(first.checked_add(count)?.checked_sub(1)?)?;
        (a.pack == b.pack).then(|| (a.offset, b.offset + b.stored_size))
    }
}

/// A request for consecutive chunks of one pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRange {
    pub first: u32,
    pub count: u32,
}

impl ChunkRange {
    pub fn end(&self) -> u32 {
        self.first + self.count
    }
}

/// Groups the chunks selected by `needed` into ranges of whole chunks of one
/// pack, each at most `max_bytes` stored bytes (a lone chunk may exceed it),
/// ordered by pack then offset (02 §7.4).
pub fn plan_ranges(
    table: &ChunkTable,
    mut needed: impl FnMut(u32) -> bool,
    max_bytes: u64,
) -> Vec<ChunkRange> {
    let mut ranges: Vec<ChunkRange> = Vec::new();
    let mut open: Option<(ChunkRange, u32, u64)> = None; // (range, pack, bytes)
    for (i, chunk) in table.chunks.iter().enumerate() {
        let index = u32::try_from(i).unwrap_or(u32::MAX);
        if !needed(index) {
            if let Some((range, _, _)) = open.take() {
                ranges.push(range);
            }
            continue;
        }
        match &mut open {
            Some((range, pack, bytes))
                if *pack == chunk.pack && *bytes + chunk.stored_size <= max_bytes =>
            {
                range.count += 1;
                *bytes += chunk.stored_size;
            }
            _ => {
                if let Some((range, _, _)) = open.take() {
                    ranges.push(range);
                }
                open = Some((
                    ChunkRange {
                        first: index,
                        count: 1,
                    },
                    chunk.pack,
                    chunk.stored_size,
                ));
            }
        }
    }
    if let Some((range, _, _)) = open {
        ranges.push(range);
    }
    ranges
}

#[cfg(test)]
pub(crate) mod tests {
    use vgames_core::manifest::{Chunk, File, Pack, Platform, Totals};
    use vgames_core::{Digest, Timestamp};

    use super::*;

    /// A manifest with the given file sizes laid out by vgames-core (hashes are
    /// placeholders; the table does not look at them).
    pub(crate) fn manifest(sizes: &[u64], pack_size: u64) -> Manifest {
        let layout =
            vgames_core::layout::assign_chunks(sizes, vgames_core::layout::CHUNK_SIZE).unwrap();
        let stored: Vec<u64> = layout.chunks.iter().map(|c| c.size).collect();
        let (packs, placements) = vgames_core::layout::packs(&stored, pack_size).unwrap();
        Manifest {
            format: "vgames.manifest/1".into(),
            server_id: uuid::Uuid::from_u128(1),
            package_id: uuid::Uuid::from_u128(2),
            version_id: uuid::Uuid::from_u128(3),
            sequence: 1,
            version_label: "1".into(),
            platform: Platform::LinuxX86_64,
            created_at: Timestamp::from_unix(0).unwrap(),
            chunk_size: vgames_core::layout::CHUNK_SIZE,
            totals: Totals {
                files: sizes.len() as u64,
                bytes: sizes.iter().sum(),
                chunks: stored.len() as u64,
                packs: packs.len() as u64,
            },
            packs: packs
                .iter()
                .map(|p| Pack {
                    size: p.size,
                    blake3: Digest::of(b""),
                })
                .collect(),
            chunks: layout
                .chunks
                .iter()
                .zip(&placements)
                .map(|(c, p)| Chunk {
                    pack: p.pack,
                    offset: p.offset,
                    stored_size: c.size,
                    size: c.size,
                    encoding: Encoding::Raw,
                    blake3: Digest::of(b""),
                })
                .collect(),
            files: sizes
                .iter()
                .zip(&layout.files)
                .enumerate()
                .map(|(i, (size, slot))| File {
                    path: format!("f{i:05}"),
                    size: *size,
                    blake3: Digest::of(b""),
                    executable: false,
                    chunk: slot.chunk,
                    offset: slot.offset,
                })
                .collect(),
            directories: Vec::new(),
            launch: None,
            controllers: None,
            saves: None,
            multiplayer: None,
        }
    }

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn extents_match_the_core_layout() {
        let sizes = [0, 10, 5 * MIB, 3, 4 * MIB, MIB, 0, 3 * MIB + 1];
        let m = manifest(&sizes, 256 * MIB);
        let table = ChunkTable::new(&m).unwrap();
        let layout =
            vgames_core::layout::assign_chunks(&sizes, vgames_core::layout::CHUNK_SIZE).unwrap();
        for c in 0..table.len() {
            let ours: Vec<_> = table
                .extents(c)
                .iter()
                .map(|e| (e.file, e.file_offset, e.len))
                .collect();
            assert_eq!(ours, layout.extents(&sizes, c), "chunk {c}");
            let covered: u64 = table.extents(c).iter().map(|e| e.len).sum();
            assert_eq!(covered, table.chunk(c).unwrap().size);
        }
    }

    #[test]
    fn inconsistent_layouts_are_refused() {
        let mut m = manifest(&[10, 20], 256 * MIB);
        m.files[1].offset = 11;
        assert!(ChunkTable::new(&m).is_err());
    }

    #[test]
    fn ranges_split_on_gaps_packs_and_size() {
        // 12 chunks of 4 MiB in packs of 5 chunks.
        let m = manifest(&[48 * MIB], 20 * MIB);
        let table = ChunkTable::new(&m).unwrap();
        assert_eq!(table.pack_count(), 3);
        let all = plan_ranges(&table, |_| true, 32 * MIB);
        assert_eq!(
            all,
            vec![
                ChunkRange { first: 0, count: 5 },
                ChunkRange { first: 5, count: 5 },
                ChunkRange {
                    first: 10,
                    count: 2
                },
            ]
        );
        let small = plan_ranges(&table, |c| c != 2, 8 * MIB);
        assert_eq!(
            small,
            vec![
                ChunkRange { first: 0, count: 2 },
                ChunkRange { first: 3, count: 2 },
                ChunkRange { first: 5, count: 2 },
                ChunkRange { first: 7, count: 2 },
                ChunkRange { first: 9, count: 1 },
                ChunkRange {
                    first: 10,
                    count: 2
                },
            ]
        );
        assert_eq!(table.byte_range(5, 2), Some((0, 8 * MIB)));
        assert_eq!(table.byte_range(4, 2), None, "crosses a pack boundary");
    }
}
