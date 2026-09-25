//! Chunk encoding (02-package-format §4 step 4) and bounded decoding (§7.5).

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// zstd level used by the packer. Output is deterministic for a given input
/// and level, so a resumed upload regenerates identical bytes.
pub const ZSTD_LEVEL: i32 = 3;

/// zstd's worst-case expansion bound accepted by the manifest validator.
pub use vgames_core::layout::MAX_STORED_OVERHEAD;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    Raw,
    Zstd,
}

/// Packer compression option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    /// Every chunk stored raw (the browser packer, and fastest to pack).
    #[default]
    None,
    /// zstd when the frame is at least 10% smaller than raw.
    Auto,
}

/// A chunk ready to be written into a pack.
#[derive(Debug)]
pub struct EncodedChunk<'a> {
    pub encoding: Encoding,
    pub stored: Cow<'a, [u8]>,
    /// BLAKE3 of the decoded bytes.
    pub blake3: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChunkError {
    #[error("chunk stored size is {found} bytes, expected {expected}")]
    StoredSize { expected: u64, found: u64 },
    #[error("chunk decodes to more than its declared {size} bytes or is not a valid zstd frame")]
    Decode { size: u64 },
    #[error("chunk decodes to {found} bytes, expected {expected}")]
    DecodedSize { expected: u64, found: u64 },
    #[error("chunk hash does not match the manifest")]
    HashMismatch,
    #[error("zstd chunks are not supported in this build")]
    ZstdUnsupported,
}

/// Encodes a chunk's decoded bytes per `compression`.
pub fn encode_chunk(decoded: &[u8], compression: Compression) -> EncodedChunk<'_> {
    let blake3 = *blake3::hash(decoded).as_bytes();
    if compression == Compression::Auto
        && let Some(frame) = compress(decoded)
        // Keep zstd only if it saves at least 10%.
        && (frame.len() as u128) * 10 <= (decoded.len() as u128) * 9
    {
        return EncodedChunk {
            encoding: Encoding::Zstd,
            stored: Cow::Owned(frame),
            blake3,
        };
    }
    EncodedChunk {
        encoding: Encoding::Raw,
        stored: Cow::Borrowed(decoded),
        blake3,
    }
}

/// Re-encodes a chunk with a known encoding (resumed uploads, second pass).
pub fn encode_as(decoded: &[u8], encoding: Encoding) -> Result<Cow<'_, [u8]>, ChunkError> {
    match encoding {
        Encoding::Raw => Ok(Cow::Borrowed(decoded)),
        Encoding::Zstd => compress(decoded)
            .map(Cow::Owned)
            .ok_or(ChunkError::ZstdUnsupported),
    }
}

#[cfg(feature = "zstd")]
fn compress(decoded: &[u8]) -> Option<Vec<u8>> {
    zstd::bulk::compress(decoded, ZSTD_LEVEL).ok()
}

#[cfg(not(feature = "zstd"))]
fn compress(_decoded: &[u8]) -> Option<Vec<u8>> {
    None
}

/// Expected properties of one chunk, from the manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedChunk {
    pub encoding: Encoding,
    pub stored_size: u64,
    pub size: u64,
    pub blake3: [u8; 32],
}

/// Decodes `stored` into `out` (cleared first) and checks size and BLAKE3.
/// zstd output is bounded to `expected.size`, so a decompression bomb fails
/// without allocating. On success `out` holds exactly the decoded bytes.
pub fn decode_and_verify(
    expected: &ExpectedChunk,
    stored: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), ChunkError> {
    if stored.len() as u64 != expected.stored_size {
        return Err(ChunkError::StoredSize {
            expected: expected.stored_size,
            found: stored.len() as u64,
        });
    }
    out.clear();
    match expected.encoding {
        Encoding::Raw => out.extend_from_slice(stored),
        Encoding::Zstd => decompress_bounded(stored, expected.size, out)?,
    }
    if out.len() as u64 != expected.size {
        return Err(ChunkError::DecodedSize {
            expected: expected.size,
            found: out.len() as u64,
        });
    }
    if blake3::hash(out).as_bytes() != &expected.blake3 {
        return Err(ChunkError::HashMismatch);
    }
    Ok(())
}

#[cfg(feature = "zstd")]
fn decompress_bounded(stored: &[u8], size: u64, out: &mut Vec<u8>) -> Result<(), ChunkError> {
    let capacity = usize::try_from(size).map_err(|_| ChunkError::Decode { size })?;
    out.reserve(capacity);
    let mut decoder = zstd::bulk::Decompressor::new().map_err(|_| ChunkError::Decode { size })?;
    // Writes at most `out.capacity()` bytes; a larger output is an error.
    decoder
        .decompress_to_buffer(stored, out)
        .map_err(|_| ChunkError::Decode { size })?;
    if out.len() > capacity {
        return Err(ChunkError::Decode { size });
    }
    Ok(())
}

#[cfg(not(feature = "zstd"))]
fn decompress_bounded(_stored: &[u8], _size: u64, _out: &mut Vec<u8>) -> Result<(), ChunkError> {
    Err(ChunkError::ZstdUnsupported)
}

#[cfg(all(test, feature = "zstd"))]
mod tests {
    use super::*;

    fn expected_for(decoded: &[u8], encoded: &EncodedChunk<'_>) -> ExpectedChunk {
        ExpectedChunk {
            encoding: encoded.encoding,
            stored_size: encoded.stored.len() as u64,
            size: decoded.len() as u64,
            blake3: encoded.blake3,
        }
    }

    #[test]
    fn compressible_data_uses_zstd_and_round_trips() {
        let decoded = vec![b'a'; 100_000];
        let encoded = encode_chunk(&decoded, Compression::Auto);
        assert_eq!(encoded.encoding, Encoding::Zstd);
        assert!(encoded.stored.len() < 1000);
        let mut out = Vec::new();
        decode_and_verify(&expected_for(&decoded, &encoded), &encoded.stored, &mut out).unwrap();
        assert_eq!(out, decoded);
        assert_eq!(encode_as(&decoded, Encoding::Zstd).unwrap(), encoded.stored);
    }

    #[test]
    fn incompressible_data_stays_raw() {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let decoded: Vec<u8> = (0..100_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let encoded = encode_chunk(&decoded, Compression::Auto);
        assert_eq!(encoded.encoding, Encoding::Raw);
        assert_eq!(
            encode_chunk(&[b'a'; 1000], Compression::None).encoding,
            Encoding::Raw
        );
    }

    #[test]
    fn decompression_bomb_is_bounded() {
        let big = vec![0u8; 8 * 1024 * 1024];
        let frame = zstd::bulk::compress(&big, 3).unwrap();
        let expected = ExpectedChunk {
            encoding: Encoding::Zstd,
            stored_size: frame.len() as u64,
            size: 4096,
            blake3: [0; 32],
        };
        let mut out = Vec::new();
        assert_eq!(
            decode_and_verify(&expected, &frame, &mut out),
            Err(ChunkError::Decode { size: 4096 })
        );
        assert!(out.capacity() < 1024 * 1024);
    }

    #[test]
    fn corruption_is_detected() {
        let decoded = vec![7u8; 5000];
        let encoded = encode_chunk(&decoded, Compression::None);
        let expected = expected_for(&decoded, &encoded);
        let mut flipped = encoded.stored.to_vec();
        flipped[10] ^= 1;
        let mut out = Vec::new();
        assert_eq!(
            decode_and_verify(&expected, &flipped, &mut out),
            Err(ChunkError::HashMismatch)
        );
        assert!(matches!(
            decode_and_verify(&expected, &flipped[1..], &mut out),
            Err(ChunkError::StoredSize { .. })
        ));
        let zstd = encode_chunk(&decoded, Compression::Auto);
        let expected = expected_for(&decoded, &zstd);
        let mut garbage = zstd.stored.to_vec();
        garbage[0] ^= 0xff;
        assert!(decode_and_verify(&expected, &garbage, &mut out).is_err());
    }
}
