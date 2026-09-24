//! File hashes assembled from per-chunk pieces.
//!
//! Packs upload in parallel, so a large file's chunks are read out of order.
//! Each exclusive chunk starts at a multiple of the chunk size (4 MiB, a power
//! of two times BLAKE3's 1 KiB chunk), so it is a complete BLAKE3 subtree: its
//! chaining value can be computed alone and the file's root hash merged later.
//! The result equals `blake3::hash(whole file)` (tested below).

use blake3::hazmat::{
    ChainingValue, HasherExt, Mode, merge_subtrees_non_root, merge_subtrees_root,
};

/// Chaining value of one chunk of a large file, `piece` chunks into the file.
/// `chunk_size` must be a power of two ≥ 1024.
pub fn piece_chaining_value(piece: u64, chunk_size: u64, bytes: &[u8]) -> ChainingValue {
    let mut hasher = blake3::Hasher::new();
    hasher.set_input_offset(piece * chunk_size);
    hasher.update(bytes);
    hasher.finalize_non_root()
}

/// Root hash of a file made of `pieces` (in order). Every piece is a full
/// `chunk_size` except possibly the last. With a single piece, pass the
/// piece's own hash instead (a one-chunk file is hashed directly).
pub fn merge_pieces(pieces: &[ChainingValue]) -> Option<blake3::Hash> {
    match pieces {
        [] | [_] => None,
        _ => {
            let (left, right) = split(pieces);
            Some(merge_subtrees_root(
                &subtree(left),
                &subtree(right),
                Mode::Hash,
            ))
        }
    }
}

/// BLAKE3's left subtree holds the largest power of two of pieces that is
/// strictly less than the total (valid because pieces are full except the last).
fn split(pieces: &[ChainingValue]) -> (&[ChainingValue], &[ChainingValue]) {
    pieces.split_at(largest_pow2_below(pieces.len()))
}

fn largest_pow2_below(n: usize) -> usize {
    let mut p = 1;
    while p * 2 < n {
        p *= 2;
    }
    p
}

fn subtree(pieces: &[ChainingValue]) -> ChainingValue {
    match pieces {
        [one] => *one,
        _ => {
            let (left, right) = split(pieces);
            merge_subtrees_non_root(&subtree(left), &subtree(right), Mode::Hash)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHUNK: u64 = 4096;

    fn data(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u32).wrapping_mul(2_654_435_761).to_le_bytes()[1])
            .collect()
    }

    #[test]
    fn merged_hash_equals_direct_hash() {
        for len in [
            2 * CHUNK as usize,
            2 * CHUNK as usize + 1,
            3 * CHUNK as usize,
            5 * CHUNK as usize - 7,
            8 * CHUNK as usize,
            9 * CHUNK as usize + 100,
            17 * CHUNK as usize,
        ] {
            let bytes = data(len);
            let pieces: Vec<_> = bytes
                .chunks(CHUNK as usize)
                .enumerate()
                .map(|(i, p)| piece_chaining_value(i as u64, CHUNK, p))
                .collect();
            assert_eq!(
                merge_pieces(&pieces),
                Some(blake3::hash(&bytes)),
                "len {len}"
            );
        }
    }

    #[test]
    fn split_matches_blake3_tree() {
        for n in 2..70usize {
            let pieces = vec![[0u8; 32]; n];
            let (l, r) = split(&pieces);
            assert_eq!(l.len(), largest_pow2_below(n), "n = {n}");
            assert!(!r.is_empty());
        }
    }
}
