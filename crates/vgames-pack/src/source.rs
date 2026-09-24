//! Pack byte generation from source files, with hashing in the same pass.
//!
//! [`PackSource`] produces the exact stored bytes of any pack from any offset
//! by reading the source files through a [`SourceReader`] (native files, or
//! anything else that can read at an offset). Pack bytes are never written to
//! disk. While producing bytes it records chunk hashes, file hashes and pack
//! hashes in a shared [`HashCollector`], so several packs can stream in
//! parallel and the manifest is built from the collector afterwards.

use std::io::{self, Read};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use blake3::hazmat::ChainingValue;

use crate::encode::{self, Compression, Encoding};
use crate::hash;
use crate::plan::{Packing, Plan, SourceFile};

/// Reads source file bytes.
pub trait SourceReader: Send + Sync {
    /// Fills `buf` with the bytes of `file` (index `index` in the plan)
    /// starting at `offset`. Must fail with [`SourceError::Changed`] (wrapped
    /// in `io::Error`) when the file no longer matches its planned size or mtime.
    fn read_exact_at(
        &self,
        index: u32,
        file: &SourceFile,
        offset: u64,
        buf: &mut [u8],
    ) -> io::Result<()>;
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("{path} changed while packing; start the upload again")]
    Changed { path: String },
    #[error("chunk {chunk} no longer produces the bytes planned for it (source changed)")]
    ChunkChanged { chunk: u32 },
    #[error("no pack {0}")]
    NoSuchPack(u32),
    #[error("missing hashes for {0}; every pack must be generated before building the manifest")]
    Incomplete(&'static str),
    #[error("chunk encoding failed")]
    Encode(#[from] encode::ChunkError),
    #[error("cannot read the source")]
    Io(#[from] io::Error),
}

impl SourceError {
    fn into_io(self) -> io::Error {
        match self {
            SourceError::Io(e) => e,
            other => io::Error::other(other),
        }
    }
}

/// Reads the decoded bytes of `chunk` into `buf` (cleared first).
pub fn read_chunk(
    plan: &Plan,
    reader: &dyn SourceReader,
    chunk: u32,
    buf: &mut Vec<u8>,
) -> io::Result<()> {
    let len = usize::try_from(plan.chunk_len(chunk)).map_err(io::Error::other)?;
    buf.clear();
    buf.resize(len, 0);
    let mut at = 0usize;
    for (file, offset, n) in plan.extents(chunk) {
        let n = usize::try_from(n).map_err(io::Error::other)?;
        let source = plan
            .files()
            .get(file as usize)
            .ok_or_else(|| io::Error::other("extent outside plan"))?;
        let target = buf
            .get_mut(at..at + n)
            .ok_or_else(|| io::Error::other("extent outside chunk"))?;
        reader.read_exact_at(file, source, offset, target)?;
        at += n;
    }
    Ok(())
}

/// Every hash the manifest needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hashes {
    pub chunks: Vec<[u8; 32]>,
    pub files: Vec<[u8; 32]>,
    pub packs: Vec<[u8; 32]>,
}

#[derive(Debug, Default)]
struct HashState {
    /// Hashes from the analysis pass; the upload pass must reproduce them.
    expected: Vec<Option<[u8; 32]>>,
    chunks: Vec<Option<[u8; 32]>>,
    files: Vec<Option<[u8; 32]>>,
    /// Per large file (by file index): chaining values of its chunks.
    pieces: Vec<Option<Vec<Option<ChainingValue>>>>,
    packs: Vec<Option<[u8; 32]>>,
}

/// Thread-safe collector of hashes produced while packs stream.
#[derive(Debug)]
pub struct HashCollector {
    state: Mutex<HashState>,
}

impl HashCollector {
    pub fn new(plan: &Plan, pack_count: u32) -> Self {
        let empty = *blake3::hash(b"").as_bytes();
        let files = plan
            .files()
            .iter()
            .map(|f| (f.size == 0).then_some(empty))
            .collect();
        let pieces = plan
            .files()
            .iter()
            .map(|f| {
                let n = f.size.div_ceil(plan.chunk_size());
                (f.size > plan.chunk_size()).then(|| vec![None; usize::try_from(n).unwrap_or(0)])
            })
            .collect();
        Self {
            state: Mutex::new(HashState {
                expected: vec![None; plan.chunk_count() as usize],
                chunks: vec![None; plan.chunk_count() as usize],
                files,
                pieces,
                packs: vec![None; pack_count as usize],
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a chunk's decoded bytes. A chunk seen before (resumed upload)
    /// must hash the same, or the source changed.
    pub fn record_chunk(
        &self,
        plan: &Plan,
        chunk: u32,
        decoded: &[u8],
        blake3: [u8; 32],
    ) -> Result<(), SourceError> {
        let mut state = self.lock();
        if state
            .expected
            .get(chunk as usize)
            .copied()
            .flatten()
            .is_some_and(|e| e != blake3)
        {
            return Err(SourceError::ChunkChanged { chunk });
        }
        let slot = state
            .chunks
            .get_mut(chunk as usize)
            .ok_or(SourceError::ChunkChanged { chunk })?;
        match slot {
            Some(previous) if *previous != blake3 => {
                return Err(SourceError::ChunkChanged { chunk });
            }
            Some(_) => return Ok(()),
            None => *slot = Some(blake3),
        }
        let Some(info) = plan.layout().chunks.get(chunk as usize) else {
            return Ok(());
        };
        if info.exclusive {
            let file = info.first_file as usize;
            let first = plan
                .layout()
                .files
                .get(file)
                .and_then(|f| f.chunk)
                .unwrap_or(chunk);
            let piece = chunk - first;
            let chunk_size = plan.chunk_size();
            match state.pieces.get_mut(file) {
                Some(Some(pieces)) => {
                    let cv = hash::piece_chaining_value(u64::from(piece), chunk_size, decoded);
                    if let Some(p) = pieces.get_mut(piece as usize) {
                        *p = Some(cv);
                    }
                    if pieces.iter().all(Option::is_some) {
                        let cvs: Vec<ChainingValue> = pieces.iter().flatten().copied().collect();
                        if let (Some(root), Some(f)) =
                            (hash::merge_pieces(&cvs), state.files.get_mut(file))
                        {
                            *f = Some(*root.as_bytes());
                        }
                    }
                }
                // A file of exactly one chunk: its hash is the chunk hash.
                _ => {
                    if let Some(f) = state.files.get_mut(file) {
                        *f = Some(blake3);
                    }
                }
            }
        } else {
            let mut at = 0usize;
            for (file, _, len) in plan.extents(chunk) {
                let len = usize::try_from(len).unwrap_or(0);
                if let (Some(bytes), Some(f)) = (
                    decoded.get(at..at + len),
                    state.files.get_mut(file as usize),
                ) {
                    *f = Some(*blake3::hash(bytes).as_bytes());
                }
                at += len;
            }
        }
        Ok(())
    }

    pub fn record_pack(&self, pack: u32, blake3: [u8; 32]) {
        if let Some(p) = self.lock().packs.get_mut(pack as usize) {
            *p = Some(blake3);
        }
    }

    pub fn pack_hash(&self, pack: u32) -> Option<[u8; 32]> {
        self.lock().packs.get(pack as usize).copied().flatten()
    }

    /// All hashes, once every pack has been generated.
    pub fn finish(&self) -> Result<Hashes, SourceError> {
        let state = self.lock();
        let all = |v: &Vec<Option<[u8; 32]>>, what| {
            v.iter()
                .copied()
                .collect::<Option<Vec<_>>>()
                .ok_or(SourceError::Incomplete(what))
        };
        Ok(Hashes {
            chunks: all(&state.chunks, "chunks")?,
            files: all(&state.files, "files")?,
            packs: all(&state.packs, "packs")?,
        })
    }
}

/// Everything needed to generate pack bytes.
#[derive(Clone)]
pub struct PackSource {
    pub plan: Arc<Plan>,
    pub packing: Arc<Packing>,
    pub reader: Arc<dyn SourceReader>,
    pub hashes: Arc<HashCollector>,
}

impl PackSource {
    pub fn new(plan: Arc<Plan>, packing: Arc<Packing>, reader: Arc<dyn SourceReader>) -> Self {
        let hashes = Arc::new(HashCollector::new(&plan, packing.pack_count()));
        Self {
            plan,
            packing,
            reader,
            hashes,
        }
    }

    /// A reader producing pack `pack`'s bytes from `start` to its end. Bytes
    /// before `start` are regenerated (for the pack hash) but not returned.
    pub fn open_pack(&self, pack: u32, start: u64) -> Result<PackReader, SourceError> {
        let slot = *self
            .packing
            .packs
            .get(pack as usize)
            .ok_or(SourceError::NoSuchPack(pack))?;
        Ok(PackReader {
            source: self.clone(),
            pack,
            next_chunk: slot.first_chunk,
            end_chunk: slot.first_chunk + slot.chunk_count,
            decoded: Vec::new(),
            stored: Vec::new(),
            stored_is_decoded: true,
            pos: 0,
            skip: start.min(slot.size),
            hasher: blake3::Hasher::new(),
            done: false,
        })
    }

    pub fn pack_size(&self, pack: u32) -> Option<u64> {
        self.packing.packs.get(pack as usize).map(|p| p.size)
    }
}

/// `Read` over one pack's stored bytes. Blocking: run it off the async runtime.
pub struct PackReader {
    source: PackSource,
    pack: u32,
    next_chunk: u32,
    end_chunk: u32,
    decoded: Vec<u8>,
    stored: Vec<u8>,
    /// Raw chunks are served straight from `decoded` (no copy).
    stored_is_decoded: bool,
    pos: usize,
    skip: u64,
    hasher: blake3::Hasher,
    done: bool,
}

impl PackReader {
    fn current(&self) -> &[u8] {
        if self.stored_is_decoded {
            &self.decoded
        } else {
            &self.stored
        }
    }

    /// Loads the next chunk; returns false at the end of the pack.
    fn load_next(&mut self) -> Result<bool, SourceError> {
        if self.next_chunk >= self.end_chunk {
            if !self.done {
                self.done = true;
                let hash = *self.hasher.finalize().as_bytes();
                self.source.hashes.record_pack(self.pack, hash);
            }
            return Ok(false);
        }
        let chunk = self.next_chunk;
        self.next_chunk += 1;
        let plan = &self.source.plan;
        read_chunk(plan, self.source.reader.as_ref(), chunk, &mut self.decoded)?;
        let packed = *self
            .source
            .packing
            .chunks
            .get(chunk as usize)
            .ok_or(SourceError::ChunkChanged { chunk })?;
        match packed.encoding {
            Encoding::Raw => self.stored_is_decoded = true,
            Encoding::Zstd => {
                let stored = encode::encode_as(&self.decoded, Encoding::Zstd)?;
                self.stored.clear();
                self.stored.extend_from_slice(&stored);
                self.stored_is_decoded = false;
            }
        }
        if self.current().len() as u64 != packed.stored_size {
            return Err(SourceError::ChunkChanged { chunk });
        }
        let chunk_hash = *blake3::hash(&self.decoded).as_bytes();
        self.source
            .hashes
            .record_chunk(plan, chunk, &self.decoded, chunk_hash)?;
        // The pack hash covers every stored byte, including skipped ones.
        let current = if self.stored_is_decoded {
            &self.decoded
        } else {
            &self.stored
        };
        self.hasher.update(current);
        self.pos = 0;
        Ok(true)
    }
}

impl Read for PackReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            let len = self.current().len();
            if self.pos >= len {
                if !self.load_next().map_err(SourceError::into_io)? {
                    return Ok(0);
                }
                continue;
            }
            if self.skip > 0 {
                let n = (len - self.pos).min(usize::try_from(self.skip).unwrap_or(usize::MAX));
                self.pos += n;
                self.skip -= n as u64;
                continue;
            }
            let available = self.current().get(self.pos..).unwrap_or_default();
            let n = available.len().min(out.len());
            if let (Some(dst), Some(src)) = (out.get_mut(..n), available.get(..n)) {
                dst.copy_from_slice(src);
            }
            self.pos += n;
            return Ok(n);
        }
    }
}

/// Analysis result for one chunk: encoding, stored size, BLAKE3.
type ChunkAnalysis = (Encoding, u64, [u8; 32]);

/// First pass for `compression = auto`: encodes every chunk to learn its
/// stored size, using `threads` threads. Hashes go into a fresh collector
/// that the returned [`PackSource`] reuses, so the upload pass then detects
/// any byte that changed in between.
pub fn analyze(
    plan: Arc<Plan>,
    reader: Arc<dyn SourceReader>,
    compression: Compression,
    pack_size: u64,
    threads: usize,
) -> Result<PackSource, SourceError> {
    let count = plan.chunk_count();
    let results: Mutex<Vec<Option<ChunkAnalysis>>> = Mutex::new(vec![None; count as usize]);
    let next = std::sync::atomic::AtomicU32::new(0);
    let failure: Mutex<Option<SourceError>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..threads.max(1) {
            scope.spawn(|| {
                let mut buf = Vec::new();
                loop {
                    let chunk = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if chunk >= count
                        || failure
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .is_some()
                    {
                        return;
                    }
                    if let Err(e) = read_chunk(&plan, reader.as_ref(), chunk, &mut buf) {
                        *failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.into());
                        return;
                    }
                    let encoded = encode::encode_chunk(&buf, compression);
                    let entry = (
                        encoded.encoding,
                        encoded.stored.len() as u64,
                        encoded.blake3,
                    );
                    if let Some(slot) = results
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .get_mut(chunk as usize)
                    {
                        *slot = Some(entry);
                    }
                }
            });
        }
    });
    if let Some(error) = failure.into_inner().unwrap_or_else(PoisonError::into_inner) {
        return Err(error);
    }
    let results: Vec<ChunkAnalysis> = results
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .into_iter()
        .collect::<Option<_>>()
        .ok_or(SourceError::Incomplete("chunks"))?;
    let encodings: Vec<(Encoding, u64)> = results.iter().map(|r| (r.0, r.1)).collect();
    let packing = Packing::new(&plan, &encodings, pack_size)
        .map_err(|e| SourceError::Io(io::Error::other(e)))?;
    let source = PackSource::new(plan, Arc::new(packing), reader);
    {
        let mut state = source.hashes.lock();
        for (slot, r) in state.expected.iter_mut().zip(&results) {
            *slot = Some(r.2);
        }
    }
    Ok(source)
}
