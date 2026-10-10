//! The chunk journal (02 §7.9): `.vgames/journal.bin` records which chunks are
//! durably written, so an interrupted install resumes by fetching only the
//! missing ones.
//!
//! Layout (little endian): `"VGJRNL01"` · version id (16 bytes) · manifest
//! BLAKE3 (32) · chunk count (u32) · bitset (⌈n/8⌉ bytes, bit `i % 8` of byte
//! `i / 8`) · BLAKE3 of everything before (32). A journal for another version
//! or manifest, or a damaged one, is ignored (the install starts over, which
//! is safe: chunks are rewritten in place). About 3 KB per 100 GB.
//!
//! A bit is set only after the chunk's files were fsynced (the persist order
//! is: fsync dirty files → write temp → fsync → rename).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use uuid::Uuid;

use crate::fsutil::{self, SafeRoot};

const MAGIC: &[u8; 8] = b"VGJRNL01";
const HEADER: usize = 8 + 16 + 32 + 4;

/// A fixed-size set of chunk indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitset {
    len: u32,
    words: Vec<u64>,
}

impl Bitset {
    pub fn new(len: u32) -> Self {
        Self {
            len,
            words: vec![0; (len as usize).div_ceil(64)],
        }
    }

    pub fn len(&self) -> u32 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, i: u32) -> bool {
        i < self.len
            && self
                .words
                .get(i as usize / 64)
                .is_some_and(|w| w & (1 << (i % 64)) != 0)
    }

    pub fn set(&mut self, i: u32) {
        if i < self.len
            && let Some(w) = self.words.get_mut(i as usize / 64)
        {
            *w |= 1 << (i % 64);
        }
    }

    pub fn count(&self) -> u32 {
        self.words.iter().map(|w| w.count_ones()).sum()
    }

    pub fn is_full(&self) -> bool {
        self.count() == self.len
    }

    fn to_bytes(&self) -> Vec<u8> {
        let n = (self.len as usize).div_ceil(8);
        let mut out: Vec<u8> = self.words.iter().flat_map(|w| w.to_le_bytes()).collect();
        out.truncate(n);
        out
    }

    fn from_bytes(len: u32, bytes: &[u8]) -> Option<Self> {
        if bytes.len() != (len as usize).div_ceil(8) {
            return None;
        }
        let mut set = Self::new(len);
        for (i, word) in set.words.iter_mut().enumerate() {
            let mut buf = [0u8; 8];
            let start = i * 8;
            let end = (start + 8).min(bytes.len());
            let part = bytes.get(start..end)?;
            buf.get_mut(..part.len())?.copy_from_slice(part);
            *word = u64::from_le_bytes(buf);
        }
        // Bits past `len` must be zero.
        let tail = len % 64;
        if tail != 0 && set.words.last().is_some_and(|w| w >> tail != 0) {
            return None;
        }
        Some(set)
    }
}

/// Identifies the download a journal belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalKey {
    pub version_id: Uuid,
    pub manifest_blake3: [u8; 32],
    pub chunk_count: u32,
}

/// The on-disk journal of one download.
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    /// Inside an install: read and written through the root's folder handle (INS-07).
    root: Option<(Arc<SafeRoot>, String)>,
    key: JournalKey,
    done: Bitset,
}

/// Largest journal accepted: a bit per chunk of the largest manifest, plus the header.
const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;

impl Journal {
    /// Loads the journal at `path`, or starts an empty one when it is missing,
    /// damaged or belongs to another download.
    pub fn load_or_new(path: &Path, key: JournalKey) -> Self {
        Self::load(std::fs::read(path), path.to_owned(), None, key)
    }

    /// The journal `rel` under an install root, read and persisted through the root's handle.
    pub fn in_root(root: Arc<SafeRoot>, rel: &str, key: JournalKey) -> Self {
        let read = root
            .read(rel, MAX_JOURNAL_BYTES)
            .map_err(|error| match error {
                fsutil::SafePathError::Io { source, .. } => source,
                other => std::io::Error::other(other.to_string()),
            })
            .and_then(|bytes| bytes.ok_or_else(|| std::io::ErrorKind::NotFound.into()));
        let path = root.path_of(rel);
        Self::load(read, path, Some((root, rel.to_owned())), key)
    }

    fn load(
        read: std::io::Result<Vec<u8>>,
        path: PathBuf,
        root: Option<(Arc<SafeRoot>, String)>,
        key: JournalKey,
    ) -> Self {
        let done = match read {
            Ok(bytes) => match decode(&bytes, &key) {
                Some(done) => done,
                None => {
                    tracing::warn!(path = %path.display(), "ignoring a journal from another download or a damaged one");
                    Bitset::new(key.chunk_count)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Bitset::new(key.chunk_count),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "cannot read the journal; starting over");
                Bitset::new(key.chunk_count)
            }
        };
        Self {
            path,
            root,
            key,
            done,
        }
    }

    pub fn done(&self) -> &Bitset {
        &self.done
    }

    pub fn mark(&mut self, chunk: u32) {
        self.done.set(chunk);
    }

    /// Writes the journal atomically. The caller has fsynced every file the
    /// newly marked chunks touched.
    pub fn persist(&self) -> std::io::Result<()> {
        let bytes = encode(&self.key, &self.done);
        match &self.root {
            Some((root, rel)) => root.atomic_write(rel, &bytes).map_err(|error| match error {
                fsutil::SafePathError::Io { source, .. } => source,
                other => std::io::Error::other(other.to_string()),
            }),
            None => fsutil::atomic_write(&self.path, &bytes),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn encode(key: &JournalKey, done: &Bitset) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + done.to_bytes().len() + 32);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(key.version_id.as_bytes());
    out.extend_from_slice(&key.manifest_blake3);
    out.extend_from_slice(&key.chunk_count.to_le_bytes());
    out.extend_from_slice(&done.to_bytes());
    let check = blake3::hash(&out);
    out.extend_from_slice(check.as_bytes());
    out
}

fn decode(bytes: &[u8], key: &JournalKey) -> Option<Bitset> {
    let body_len = bytes.len().checked_sub(32)?;
    let (body, check) = bytes.split_at(body_len);
    if blake3::hash(body).as_bytes() != check {
        return None;
    }
    let (header, bits) = body.split_at_checked(HEADER)?;
    let mut expected = Vec::with_capacity(HEADER);
    expected.extend_from_slice(MAGIC);
    expected.extend_from_slice(key.version_id.as_bytes());
    expected.extend_from_slice(&key.manifest_blake3);
    expected.extend_from_slice(&key.chunk_count.to_le_bytes());
    if header != expected.as_slice() {
        return None;
    }
    Bitset::from_bytes(key.chunk_count, bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u32) -> JournalKey {
        JournalKey {
            version_id: Uuid::from_u128(7),
            manifest_blake3: [3; 32],
            chunk_count: n,
        }
    }

    #[test]
    fn bitset_basics() {
        let mut b = Bitset::new(130);
        for i in [0, 63, 64, 129, 500] {
            b.set(i);
        }
        assert!(b.get(0) && b.get(63) && b.get(64) && b.get(129));
        assert!(!b.get(1) && !b.get(500));
        assert_eq!(b.count(), 4);
        let bytes = b.to_bytes();
        assert_eq!(bytes.len(), 17);
        assert_eq!(Bitset::from_bytes(130, &bytes).unwrap(), b);
        assert!(Bitset::from_bytes(129, &bytes).is_none());
    }

    #[test]
    fn persists_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.bin");
        let mut j = Journal::load_or_new(&path, key(1000));
        assert_eq!(j.done().count(), 0);
        for i in (0..1000).step_by(3) {
            j.mark(i);
        }
        j.persist().unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        assert_eq!(size as usize, HEADER + 125 + 32);
        let again = Journal::load_or_new(&path, key(1000));
        assert_eq!(again.done(), j.done());
    }

    #[test]
    fn foreign_or_damaged_journals_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.bin");
        let mut j = Journal::load_or_new(&path, key(64));
        j.mark(5);
        j.persist().unwrap();
        let other = JournalKey {
            version_id: Uuid::from_u128(8),
            ..key(64)
        };
        assert_eq!(Journal::load_or_new(&path, other).done().count(), 0);
        assert_eq!(Journal::load_or_new(&path, key(65)).done().count(), 0);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[HEADER] ^= 0x40;
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(Journal::load_or_new(&path, key(64)).done().count(), 0);
        std::fs::write(&path, b"short").unwrap();
        assert_eq!(Journal::load_or_new(&path, key(64)).done().count(), 0);
    }

    proptest::proptest! {
        #[test]
        fn decoding_arbitrary_bytes_never_panics(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..300), n in 0u32..2000) {
            let _ = decode(&bytes, &key(n));
        }
    }
}
