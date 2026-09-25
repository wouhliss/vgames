//! Runtime-catalog test vectors (A5-T12) for the launcher's runtime manager (Agent 2,
//! A2-T16/T17): `tests/vectors/runtimes/`, described in its README.md. This test pins
//! the expected outcome of each vector; `regenerate_vectors` (ignored) rebuilds them
//! with a fresh throwaway key that is never written to disk.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::io::Cursor;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use vgames_core::runtimes::{RuntimeId, RuntimesError, verify_catalog};

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/runtimes")
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn text(name: &str) -> String {
    String::from_utf8(read(name)).unwrap()
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn the_vectors_have_the_documented_outcomes() {
    let key = text("runtime-catalog-test.pub");
    // The valid current catalog.
    let v2 = read("runtimes-v2.json");
    let verified = verify_catalog(&v2, &text("runtimes-v2.json.minisig"), &key, Some(1)).unwrap();
    assert!(verified.newer);
    assert_eq!(verified.catalog.version, 2);
    // An older, genuinely signed catalog is a rollback once v2 was seen.
    let v1 = read("runtimes-v1.json");
    verify_catalog(&v1, &text("runtimes-v1.json.minisig"), &key, None).unwrap();
    assert_eq!(
        verify_catalog(&v1, &text("runtimes-v1.json.minisig"), &key, Some(2)),
        Err(RuntimesError::Rollback { got: 1, seen: 2 })
    );
    // One changed byte (a runtime's sha256) under v2's signature.
    assert_eq!(
        verify_catalog(
            &read("runtimes-v2-tampered.json"),
            &text("runtimes-v2.json.minisig"),
            &key,
            None
        ),
        Err(RuntimesError::BadSignature)
    );
    // v2 signed by a key the launcher does not know.
    assert_eq!(
        verify_catalog(&v2, &text("runtimes-v2.json.other-key.minisig"), &key, None),
        Err(RuntimesError::BadSignature)
    );
    // The archive pinned by v2 matches; the tampered copy does not.
    let entry = verified
        .catalog
        .runtimes
        .iter()
        .find(|r| r.id == RuntimeId::UmuLauncher)
        .unwrap();
    let archive = read("test-runtime.tar.gz");
    assert_eq!(entry.sha256, sha256_hex(&archive));
    assert_eq!(entry.size, archive.len() as u64);
    let tampered = read("test-runtime-tampered.tar.gz");
    assert_eq!(tampered.len(), archive.len());
    assert_ne!(entry.sha256, sha256_hex(&tampered));
}

fn catalog(version: u64, archive: &[u8], extra: bool) -> Vec<u8> {
    let mut runtimes = vec![serde_json::json!({
        "id": "umu-launcher", "version": "0.0.0-test", "os": "linux", "arch": "x86_64",
        "url": "https://github.com/wouhliss/vgames/releases/download/runtimes-test/test-runtime.tar.gz",
        "sha256": sha256_hex(archive), "size": archive.len(), "archive": "tar.gz",
        "license": "MIT", "min_launcher_version": "0.1.0"
    })];
    if extra {
        runtimes.push(serde_json::json!({
            "id": "d3dmetal", "version": "3.0", "os": "macos", "arch": "aarch64",
            "url": "https://github.com/wouhliss/vgames/releases/download/runtimes/D3DMetal-3.0.tar.gz",
            "sha256": "b".repeat(64), "size": 90_000_000u64, "archive": "tar.gz",
            "license": "LicenseRef-Apple-GPTK", "min_launcher_version": "0.1.0",
            "redistribution": "non-commercial", "macos_max_supported": 27
        }));
    }
    serde_json::to_vec(&serde_json::json!({
        "format": "vgames.runtimes/1", "version": version, "generated_at": "2026-09-25T12:00:00Z",
        "commercial": false, "runtimes": runtimes
    }))
    .unwrap()
}

fn sign(key: &minisign::SecretKey, data: &[u8]) -> String {
    minisign::sign(
        None,
        key,
        Cursor::new(data),
        Some("vgames.runtimes/1 test vector"),
        None,
    )
    .unwrap()
    .to_string()
}

/// Rebuilds the vectors: `cargo test -p vgames-core --test runtimes_vectors -- --ignored`.
#[test]
#[ignore = "rewrites tests/vectors/runtimes with a new throwaway key"]
fn regenerate_vectors() {
    let write = |name: &str, data: &[u8]| std::fs::write(dir().join(name), data).unwrap();
    let archive = read("test-runtime.tar.gz");
    let mut tampered = archive.clone();
    let at = tampered.len() / 2;
    tampered[at] ^= 1;
    write("test-runtime-tampered.tar.gz", &tampered);

    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let other = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    write(
        "runtime-catalog-test.pub",
        pair.pk.to_box().unwrap().to_string().as_bytes(),
    );
    let v1 = catalog(1, &archive, false);
    let v2 = catalog(2, &archive, true);
    write("runtimes-v1.json", &v1);
    write("runtimes-v1.json.minisig", sign(&pair.sk, &v1).as_bytes());
    write("runtimes-v2.json", &v2);
    write("runtimes-v2.json.minisig", sign(&pair.sk, &v2).as_bytes());
    write(
        "runtimes-v2.json.other-key.minisig",
        sign(&other.sk, &v2).as_bytes(),
    );
    let digest = sha256_hex(&archive);
    let last = if digest.ends_with('0') { '1' } else { '0' };
    let changed = String::from_utf8(v2.clone()).unwrap().replacen(
        &digest,
        &format!("{}{last}", &digest[..63]),
        1,
    );
    assert_ne!(changed.as_bytes(), v2.as_slice());
    write("runtimes-v2-tampered.json", changed.as_bytes());
}
