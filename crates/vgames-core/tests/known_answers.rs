//! Official BLAKE3 test vectors (`test_vectors/test_vectors.json` from the
//! BLAKE3 repository, CC0). Ed25519 RFC 8032 vectors run as unit tests in
//! `sign.rs` because they exercise the raw (non-domain-separated) layer.

use vgames_core::Digest;

#[derive(serde::Deserialize)]
struct Case {
    input_len: usize,
    hash: String,
}

#[derive(serde::Deserialize)]
struct Vectors {
    cases: Vec<Case>,
}

#[test]
fn blake3_official_vectors() {
    let v: Vectors = serde_json::from_str(include_str!("vectors/blake3.json")).unwrap();
    assert!(v.cases.len() > 30);
    for case in v.cases {
        // The official input is the byte sequence 0, 1, …, 250, 0, 1, … of the given length.
        let input: Vec<u8> = (0..case.input_len).map(|i| (i % 251) as u8).collect();
        let expected = &case.hash[..64];
        assert_eq!(
            Digest::of(&input).to_hex(),
            expected,
            "len {}",
            case.input_len
        );
        // Streaming (chunked) hashing agrees with one-shot hashing.
        let mut h = blake3::Hasher::new();
        for part in input.chunks(1000) {
            h.update(part);
        }
        assert_eq!(Digest::from(h.finalize()).to_hex(), expected);
    }
}
