//! Streaming verification of pack bytes against a manifest.
//!
//! [`PackStreamVerifier`] takes a pack's stored bytes in arbitrary pieces
//! (as they arrive from storage) and reports each chunk as soon as it is
//! complete: decoded (zstd bounded to the declared size) and BLAKE3-checked.
//! At the end it checks the pack length and hash. Used by the API's
//! `version.verify` job (Agent 1) and by the launcher's downloader.
//!
//! Only feed it expectations from a manifest whose signature was verified.

use serde::Deserialize;

use crate::encode::{self, ChunkError, Encoding, ExpectedChunk};

/// Result for one chunk. On success, `decoded` holds its plain bytes.
#[derive(Debug)]
pub struct ChunkVerdict<'a> {
    /// Chunk index in the manifest.
    pub index: u32,
    pub result: Result<&'a [u8], ChunkError>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PackError {
    #[error("the pack has more bytes than the manifest declares ({size})")]
    TooLong { size: u64 },
    #[error("the pack ended after {received} of {size} bytes")]
    Truncated { received: u64, size: u64 },
    #[error("the pack hash does not match the manifest")]
    HashMismatch,
}

/// Summary after the last byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackReport {
    pub bytes: u64,
    pub chunks_ok: u32,
    /// First failing chunk and why, if any.
    pub first_failure: Option<(u32, ChunkError)>,
}

/// What a verifier needs to know about one pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackExpectation {
    pub index: u32,
    pub size: u64,
    pub blake3: [u8; 32],
    pub first_chunk: u32,
    pub chunks: Vec<ExpectedChunk>,
}

pub struct PackStreamVerifier {
    expectation: PackExpectation,
    /// Index into `expectation.chunks` of the chunk being filled.
    current: usize,
    stored: Vec<u8>,
    decoded: Vec<u8>,
    hasher: blake3::Hasher,
    received: u64,
    chunks_ok: u32,
    first_failure: Option<(u32, ChunkError)>,
}

impl PackStreamVerifier {
    pub fn new(expectation: PackExpectation) -> Self {
        let largest = expectation
            .chunks
            .iter()
            .map(|c| c.stored_size)
            .max()
            .unwrap_or(0);
        Self {
            stored: Vec::with_capacity(usize::try_from(largest).unwrap_or(0)),
            decoded: Vec::new(),
            expectation,
            current: 0,
            hasher: blake3::Hasher::new(),
            received: 0,
            chunks_ok: 0,
            first_failure: None,
        }
    }

    /// Feeds the next bytes; `on_chunk` runs for each chunk completed by them.
    pub fn push(
        &mut self,
        mut data: &[u8],
        mut on_chunk: impl FnMut(ChunkVerdict<'_>),
    ) -> Result<(), PackError> {
        let size = self.expectation.size;
        if self.received + data.len() as u64 > size {
            return Err(PackError::TooLong { size });
        }
        self.received += data.len() as u64;
        self.hasher.update(data);
        while !data.is_empty() {
            let Some(expected) = self.expectation.chunks.get(self.current).copied() else {
                // Sizes add up (checked at construction), so this cannot happen.
                return Err(PackError::TooLong { size });
            };
            let want =
                usize::try_from(expected.stored_size).unwrap_or(usize::MAX) - self.stored.len();
            let take = want.min(data.len());
            let (head, tail) = data.split_at(take);
            self.stored.extend_from_slice(head);
            data = tail;
            if self.stored.len() as u64 == expected.stored_size {
                let index = self.expectation.first_chunk + self.current as u32;
                let result = encode::decode_and_verify(&expected, &self.stored, &mut self.decoded);
                match &result {
                    Ok(()) => self.chunks_ok += 1,
                    Err(e) if self.first_failure.is_none() => {
                        self.first_failure = Some((index, e.clone()))
                    }
                    Err(_) => {}
                }
                on_chunk(ChunkVerdict {
                    index,
                    result: result.map(|()| self.decoded.as_slice()),
                });
                self.stored.clear();
                self.current += 1;
            }
        }
        Ok(())
    }

    /// Checks the total length and the pack hash.
    pub fn finish(self) -> Result<PackReport, PackError> {
        if self.received != self.expectation.size {
            return Err(PackError::Truncated {
                received: self.received,
                size: self.expectation.size,
            });
        }
        if self.hasher.finalize().as_bytes() != &self.expectation.blake3 {
            return Err(PackError::HashMismatch);
        }
        Ok(PackReport {
            bytes: self.received,
            chunks_ok: self.chunks_ok,
            first_failure: self.first_failure,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExpectationError {
    #[error("manifest JSON cannot be read: {0}")]
    Json(String),
    #[error("no pack {0} in the manifest")]
    NoSuchPack(u32),
    #[error("invalid hash or layout in the manifest")]
    Invalid,
}

#[derive(Deserialize)]
struct ViewPack {
    size: u64,
    blake3: String,
}

#[derive(Deserialize)]
struct ViewChunk {
    pack: u32,
    offset: u64,
    stored_size: u64,
    size: u64,
    encoding: Encoding,
    blake3: String,
}

#[derive(Deserialize)]
struct ManifestView {
    packs: Vec<ViewPack>,
    chunks: Vec<ViewChunk>,
}

fn hash32(hex_str: &str) -> Result<[u8; 32], ExpectationError> {
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex_str, &mut out).map_err(|_| ExpectationError::Invalid)?;
    Ok(out)
}

/// Expectations for every pack of a manifest, read from its (already
/// signature-verified and validated) bytes. Ignores unrelated fields.
pub fn expectations_from_manifest(
    manifest: &[u8],
) -> Result<Vec<PackExpectation>, ExpectationError> {
    let view: ManifestView =
        serde_json::from_slice(manifest).map_err(|e| ExpectationError::Json(e.to_string()))?;
    let mut packs: Vec<PackExpectation> = view
        .packs
        .iter()
        .enumerate()
        .map(|(i, p)| {
            Ok(PackExpectation {
                index: u32::try_from(i).map_err(|_| ExpectationError::Invalid)?,
                size: p.size,
                blake3: hash32(&p.blake3)?,
                first_chunk: 0,
                chunks: Vec::new(),
            })
        })
        .collect::<Result<_, ExpectationError>>()?;
    for (i, c) in view.chunks.iter().enumerate() {
        let pack = packs
            .get_mut(c.pack as usize)
            .ok_or(ExpectationError::NoSuchPack(c.pack))?;
        let expected_offset: u64 = pack.chunks.iter().map(|c| c.stored_size).sum();
        if c.offset != expected_offset {
            return Err(ExpectationError::Invalid);
        }
        if pack.chunks.is_empty() {
            pack.first_chunk = u32::try_from(i).map_err(|_| ExpectationError::Invalid)?;
        }
        pack.chunks.push(ExpectedChunk {
            encoding: c.encoding,
            stored_size: c.stored_size,
            size: c.size,
            blake3: hash32(&c.blake3)?,
        });
    }
    for pack in &packs {
        if pack.chunks.iter().map(|c| c.stored_size).sum::<u64>() != pack.size {
            return Err(ExpectationError::Invalid);
        }
    }
    Ok(packs)
}

impl PackExpectation {
    /// Expectations for one pack of a manifest.
    pub fn from_manifest(manifest: &[u8], pack: u32) -> Result<Self, ExpectationError> {
        expectations_from_manifest(manifest)?
            .into_iter()
            .nth(pack as usize)
            .ok_or(ExpectationError::NoSuchPack(pack))
    }
}
