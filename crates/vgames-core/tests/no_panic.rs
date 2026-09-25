//! No-panic properties for every parser that reads untrusted bytes
//! (01-security §9, A5-T05). Each parser gets arbitrary bytes and valid
//! documents with random byte edits (which reach much deeper than random bytes).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use proptest::prelude::*;
use vgames_core::keyfile::{KeyFile, KeyKind};
use vgames_core::sign::{Context, Envelope, SecretKey};
use vgames_core::trust::{RootPin, TrustBundle, sign_bundle, verify_bundle};
use vgames_core::{compat, manifest, runtimes};

fn key() -> SecretKey {
    SecretKey::from_seed(&[7; 32])
}

fn valid_manifest() -> Vec<u8> {
    include_bytes!("vectors/pack_manifest.json").to_vec()
}

fn valid_envelope() -> Vec<u8> {
    Envelope::sign(&key(), Context::Manifest, &valid_manifest()).to_bytes()
}

fn valid_bundle() -> Vec<u8> {
    let root = key();
    let publisher = SecretKey::from_seed(&[8; 32]).public_key();
    serde_json::to_vec(&serde_json::json!({
        "format": "vgames.trust/1",
        "server_id": "01920000-0000-7000-8000-000000000000",
        "version": 4,
        "issued_at": "2026-09-24T10:00:00Z",
        "expires_at": null,
        "root_key_id": root.public_key().key_id(),
        "publishers": [{
            "key_id": publisher.key_id(), "public_key": publisher,
            "holder_user_id": "0192aaaa-0000-7000-8000-000000000001", "label": "alice",
            "not_before": "2026-09-24T00:00:00Z", "not_after": "2028-09-24T00:00:00Z"
        }],
        "revoked": [{ "key_id": SecretKey::from_seed(&[9; 32]).public_key().key_id(),
                      "revoked_at": "2026-10-01T12:00:00Z", "reason": "stolen" }],
        "next_root": null
    }))
    .unwrap()
}

fn valid_keyfile() -> Vec<u8> {
    let at = "2026-09-24T10:00:00Z".parse().unwrap();
    KeyFile::encrypt_with(
        &key(),
        KeyKind::Publisher,
        "test",
        at,
        b"pass",
        [1; 16],
        [2; 24],
    )
    .unwrap()
    .to_bytes()
}

fn valid_compat() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "format": "vgames.compat/1", "server_id": "01920000-0000-7000-8000-000000000000",
        "package_id": "0192a6f0-1c2d-7e3f-8a9b-0c1d2e3f4a5b", "target": "linux", "revision": 3,
        "created_at": "2026-09-24T10:00:00Z",
        "applies_to": { "platform": "windows-x86_64", "min_sequence": 1, "max_sequence": null },
        "status": "verified",
        "runner": { "kind": "proton", "prefer": ["umu-proton"], "env": { "A": "1" },
                    "dll_overrides": { "d3d11": "native" }, "winetricks": ["vcrun2022"] }
    }))
    .unwrap()
}

/// A valid document with 1–8 random byte replacements, insertions or deletions.
fn mutated(base: Vec<u8>) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec((any::<prop::sample::Index>(), any::<u8>(), 0u8..3), 1..8).prop_map(
        move |edits| {
            let mut b = base.clone();
            for (at, byte, op) in edits {
                if b.is_empty() {
                    b.push(byte);
                    continue;
                }
                let i = at.index(b.len());
                match op {
                    0 => b[i] = byte,
                    1 => b.insert(i, byte),
                    _ => {
                        b.remove(i);
                    }
                }
            }
            b
        },
    )
}

fn valid_runtimes() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "format": "vgames.runtimes/1", "version": 2, "generated_at": "2026-09-25T12:00:00Z",
        "commercial": false,
        "runtimes": [{
            "id": "d3dmetal", "version": "3.0", "os": "macos", "arch": "aarch64",
            "url": "https://github.com/wouhliss/vgames/releases/download/runtimes/D3DMetal-3.0.tar.gz",
            "sha256": "b".repeat(64), "size": 90_000_000u64, "archive": "tar.gz",
            "license": "LicenseRef-Apple-GPTK", "min_launcher_version": "0.2.0",
            "redistribution": "non-commercial", "macos_max_supported": 27
        }]
    }))
    .unwrap()
}

fn arbitrary() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..1024)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

    #[test]
    fn manifest_never_panics(b in prop_oneof![arbitrary(), mutated(valid_manifest())]) {
        let _ = manifest::parse_and_validate(&b);
    }

    #[test]
    fn envelope_never_panics(b in prop_oneof![arbitrary(), mutated(valid_envelope())]) {
        if let Ok(env) = Envelope::parse(&b) {
            prop_assert_eq!(Envelope::parse(&env.to_bytes()).unwrap(), env);
        }
    }

    #[test]
    fn trust_bundle_never_panics(b in prop_oneof![arbitrary(), mutated(valid_bundle())]) {
        let _ = TrustBundle::parse_unverified(&b);
        // Correctly signed arbitrary bytes exercise every rule after the signature.
        let sig = sign_bundle(&key(), &b);
        let server = "01920000-0000-7000-8000-000000000000".parse().unwrap();
        let _ = verify_bundle(&b, &sig, &RootPin::new(key().public_key()), Some(3), server);
    }

    #[test]
    fn keyfile_never_panics(b in prop_oneof![arbitrary(), mutated(valid_keyfile())]) {
        if let Ok(f) = KeyFile::parse(&b) {
            prop_assert_eq!(KeyFile::parse(&f.to_bytes()).unwrap(), f);
        }
    }

    #[test]
    fn compat_never_panics(b in prop_oneof![arbitrary(), mutated(valid_compat())]) {
        let _ = compat::parse_and_validate(&b);
    }

    #[test]
    fn runtime_catalog_never_panics(b in prop_oneof![arbitrary(), mutated(valid_runtimes())]) {
        let _ = runtimes::parse_and_validate(&b);
        // Arbitrary signature text and keys never panic either.
        let sig = String::from_utf8_lossy(&b);
        let _ = runtimes::verify_catalog(&b, &sig, &sig, Some(1));
    }
}

#[test]
fn the_valid_documents_are_valid() {
    manifest::parse_and_validate(&valid_manifest()).unwrap();
    Envelope::parse(&valid_envelope()).unwrap();
    TrustBundle::parse_unverified(&valid_bundle()).unwrap();
    KeyFile::parse(&valid_keyfile()).unwrap();
    compat::parse_and_validate(&valid_compat()).unwrap();
    runtimes::parse_and_validate(&valid_runtimes()).unwrap();
}
