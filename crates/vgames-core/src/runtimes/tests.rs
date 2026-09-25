#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::io::Cursor;

use serde_json::{Value, json};

use super::*;

pub(crate) fn proton() -> Value {
    json!({
        "id": "ge-proton", "version": "GE-Proton10-17", "os": "linux", "arch": "x86_64",
        "url": "https://github.com/GloriousEggroll/proton-ge-custom/releases/download/GE-Proton10-17/GE-Proton10-17.tar.gz",
        "sha256": "a".repeat(64), "size": 480_000_000u64, "archive": "tar.gz",
        "license": "BSD-3-Clause AND LGPL-2.1-or-later", "min_launcher_version": "0.2.0"
    })
}

pub(crate) fn d3dmetal() -> Value {
    json!({
        "id": "d3dmetal", "version": "3.0", "os": "macos", "arch": "aarch64",
        "url": "https://github.com/wouhliss/vgames/releases/download/runtimes/D3DMetal-3.0.tar.gz",
        "sha256": "b".repeat(64), "size": 90_000_000u64, "archive": "tar.gz",
        "license": "LicenseRef-Apple-GPTK", "min_launcher_version": "0.2.0",
        "redistribution": "non-commercial", "macos_max_supported": 27
    })
}

pub(crate) fn wine() -> Value {
    json!({
        "id": "wine-macos", "version": "10.0", "os": "macos", "arch": "x86_64",
        "url": "https://github.com/Gcenx/macOS_Wine_builds/releases/download/10.0/wine-stable-10.0-osx64.tar.xz",
        "sha256": "c".repeat(64), "size": 200_000_000u64, "archive": "tar.xz",
        "license": "LGPL-2.1-or-later", "min_launcher_version": "0.2.0",
        "rosetta_required": true, "macos_max_supported": 27
    })
}

pub(crate) fn catalog(version: u64, runtimes: Vec<Value>) -> Value {
    json!({ "format": FORMAT, "version": version, "generated_at": "2026-09-25T12:00:00Z",
            "commercial": false, "runtimes": runtimes })
}

fn bytes(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
}

fn parse(v: &Value) -> Result<Catalog, RuntimesError> {
    parse_and_validate(&bytes(v))
}

/// Parses with runtimes[0] edited by `f`.
fn with_entry(f: impl FnOnce(&mut Value)) -> Result<Catalog, RuntimesError> {
    let mut rt = proton();
    f(&mut rt);
    parse(&catalog(1, vec![rt]))
}

fn entry_reason(r: Result<Catalog, RuntimesError>) -> String {
    match r {
        Err(RuntimesError::Entry { reason, .. }) => reason,
        other => panic!("expected an entry error, got {other:?}"),
    }
}

#[test]
fn a_valid_catalog_parses_and_roundtrips() {
    let doc = catalog(3, vec![proton(), d3dmetal(), wine()]);
    let c = parse(&doc).unwrap();
    assert_eq!(c.version, 3);
    assert_eq!(c.runtimes.len(), 3);
    assert_eq!(
        c.runtimes[1].redistribution,
        Some(Redistribution::NonCommercial)
    );
    assert_eq!(serde_json::to_value(&c).unwrap(), doc);
    parse(&catalog(1, vec![])).unwrap();
}

#[test]
fn rule_format_version_and_unknown_fields() {
    let mut doc = catalog(1, vec![]);
    doc["format"] = json!("vgames.runtimes/2");
    assert!(matches!(parse(&doc), Err(RuntimesError::Format(_))));
    assert_eq!(parse(&catalog(0, vec![])), Err(RuntimesError::VersionZero));
    let mut doc = catalog(1, vec![]);
    doc["extra"] = json!(1);
    assert!(matches!(parse(&doc), Err(RuntimesError::Json(_))));
    assert!(matches!(
        with_entry(|e| e["unknown"] = json!(true)),
        Err(RuntimesError::Json(_))
    ));
    assert!(matches!(
        with_entry(|e| e["id"] = json!("proton-9000")),
        Err(RuntimesError::Json(_))
    ));
    assert!(matches!(
        parse_and_validate(&vec![b' '; MAX_CATALOG_BYTES + 1]),
        Err(RuntimesError::TooLarge)
    ));
}

#[test]
fn rule_version_text() {
    for bad in [
        "",
        ".hidden",
        "a b",
        "a/b",
        "x".repeat(65).as_str(),
        "1.0\n",
    ] {
        assert!(
            entry_reason(with_entry(|e| e["version"] = json!(bad))).contains("version"),
            "{bad:?}"
        );
    }
    with_entry(|e| e["version"] = json!("UMU-Proton-9.0-3.2+build_1")).unwrap();
}

#[test]
fn rule_os_matches_the_runtime() {
    assert!(entry_reason(with_entry(|e| e["os"] = json!("macos"))).contains("wrong os"));
}

#[test]
fn rule_url_is_a_github_release_asset() {
    for bad in [
        "http://github.com/a/b/releases/download/t/f.tar.gz",
        "https://example.com/a/b/releases/download/t/f.tar.gz",
        "https://github.com.evil.example/a/b/releases/download/t/f.tar.gz",
        "https://github.com@evil.example/a/b/releases/download/t/f",
        "https://github.com/a/b/archive/refs/tags/t.tar.gz",
        "https://github.com/a/b/releases/download/t/f.tar.gz?x=1",
        "https://github.com/a/b/releases/download/../f.tar.gz",
        "https://github.com/a/b/releases/download/t/sub/f.tar.gz",
        "https://github.com/a/b/releases/download/t/%2e%2e",
        "https://github.com/a/b/releases/download/t/f g",
    ] {
        assert!(
            entry_reason(with_entry(|e| e["url"] = json!(bad))).contains("url"),
            "{bad}"
        );
    }
}

#[test]
fn rule_sha256_size_license_and_launcher_version() {
    assert!(entry_reason(with_entry(|e| e["sha256"] = json!("A".repeat(64)))).contains("sha256"));
    assert!(entry_reason(with_entry(|e| e["sha256"] = json!("a".repeat(63)))).contains("sha256"));
    assert!(entry_reason(with_entry(|e| e["size"] = json!(0))).contains("size"));
    assert!(
        entry_reason(with_entry(|e| e["size"] = json!(MAX_RUNTIME_BYTES + 1))).contains("size")
    );
    assert!(entry_reason(with_entry(|e| e["license"] = json!(""))).contains("license"));
    assert!(entry_reason(with_entry(|e| e["license"] = json!("MIT; rm -rf"))).contains("license"));
    for bad in ["1.2", "1.2.3.4", "01.2.3", "1.2.x", "v1.2.3"] {
        assert!(
            entry_reason(with_entry(|e| e["min_launcher_version"] = json!(bad)))
                .contains("min_launcher_version"),
            "{bad}"
        );
    }
}

#[test]
fn rule_macos_only_flags() {
    assert!(
        entry_reason(with_entry(|e| e["rosetta_required"] = json!(true))).contains("macOS-only")
    );
    assert!(
        entry_reason(with_entry(|e| e["macos_max_supported"] = json!(27))).contains("macOS-only")
    );
    let mut w = wine();
    w["arch"] = json!("aarch64");
    assert!(entry_reason(parse(&catalog(1, vec![w]))).contains("Rosetta"));
    let mut w = wine();
    w["macos_max_supported"] = json!(3);
    assert!(entry_reason(parse(&catalog(1, vec![w]))).contains("major version"));
}

#[test]
fn rule_d3dmetal_is_apple_silicon_and_non_commercial() {
    let mut d = d3dmetal();
    d["arch"] = json!("x86_64");
    assert!(entry_reason(parse(&catalog(1, vec![d]))).contains("Apple silicon"));
    let mut d = d3dmetal();
    d.as_object_mut().unwrap().remove("redistribution");
    assert!(entry_reason(parse(&catalog(1, vec![d]))).contains("non-commercial"));
    // The tripwire: a commercial catalog cannot ship it.
    let mut doc = catalog(1, vec![d3dmetal()]);
    doc["commercial"] = json!(true);
    assert!(entry_reason(parse(&doc)).contains("commercial"));
    // Apple's name for the architecture is accepted and normalized.
    let mut d = d3dmetal();
    d["arch"] = json!("arm64");
    let parsed = parse(&catalog(1, vec![d])).unwrap();
    assert_eq!(parsed.runtimes[0].arch, Arch::Aarch64);
    let out = serde_json::to_value(&parsed).unwrap();
    assert_eq!(out["runtimes"][0]["arch"], "aarch64");
}

#[test]
fn rule_entries_are_unique_and_bounded() {
    assert!(entry_reason(parse(&catalog(1, vec![proton(), proton()]))).contains("duplicate"));
    let many = vec![proton(); MAX_ENTRIES + 1];
    assert_eq!(parse(&catalog(1, many)), Err(RuntimesError::TooManyEntries));
}

// ---------------------------------------------------------------------------
// Signatures

pub(crate) struct Keys {
    pub secret: minisign::SecretKey,
    /// The one-line base64 public key compiled into the launcher.
    pub public: String,
}

pub(crate) fn keys() -> Keys {
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    Keys {
        public: pair.pk.to_base64(),
        secret: pair.sk,
    }
}

pub(crate) fn sign(k: &Keys, data: &[u8]) -> String {
    minisign::sign(
        None,
        &k.secret,
        Cursor::new(data),
        Some("vgames.runtimes/1"),
        None,
    )
    .unwrap()
    .to_string()
}

#[test]
fn a_signed_catalog_verifies_and_reports_whether_it_is_newer() {
    let k = keys();
    let data = bytes(&catalog(5, vec![proton(), wine()]));
    let sig = sign(&k, &data);
    let v = verify_catalog(&data, &sig, &k.public, None).unwrap();
    assert!(v.newer);
    assert_eq!(v.catalog.version, 5);
    assert!(
        verify_catalog(&data, &sig, &k.public, Some(4))
            .unwrap()
            .newer
    );
    assert!(
        !verify_catalog(&data, &sig, &k.public, Some(5))
            .unwrap()
            .newer,
        "same version: a refresh"
    );
    // The whole .pub file works as well as its base64 line.
    let pub_file = format!("untrusted comment: minisign public key\n{}\n", k.public);
    verify_catalog(&data, &sig, &pub_file, None).unwrap();
}

#[test]
fn tampering_another_key_and_rollback_are_refused() {
    let k = keys();
    let data = bytes(&catalog(5, vec![proton()]));
    let sig = sign(&k, &data);
    let mut tampered = data.clone();
    let at = tampered.len() / 2;
    tampered[at] ^= 1;
    assert_eq!(
        verify_catalog(&tampered, &sig, &k.public, None),
        Err(RuntimesError::BadSignature)
    );
    assert_eq!(
        verify_catalog(&data, &sig, &keys().public, None),
        Err(RuntimesError::BadSignature)
    );
    assert_eq!(
        verify_catalog(&data, &sig, &k.public, Some(6)),
        Err(RuntimesError::Rollback { got: 5, seen: 6 })
    );
    assert!(matches!(
        verify_catalog(&data, "not a signature", &k.public, None),
        Err(RuntimesError::SignatureMalformed(_))
    ));
    assert_eq!(
        verify_catalog(&data, &sig, "not a key", None),
        Err(RuntimesError::PublicKeyMalformed)
    );
    assert!(matches!(
        verify_catalog(&data, &"x".repeat(MAX_SIGNATURE_BYTES + 1), &k.public, None),
        Err(RuntimesError::SignatureMalformed(_))
    ));
}

#[test]
fn the_signature_is_checked_before_the_format() {
    let k = keys();
    let junk = b"{\"format\": 1}";
    // Unsigned junk: a signature error, the parser never sees it.
    assert_eq!(
        verify_catalog(junk, &sign(&keys(), junk), &k.public, None),
        Err(RuntimesError::BadSignature)
    );
    // Correctly signed but invalid: refused by the format rules.
    assert!(matches!(
        verify_catalog(junk, &sign(&k, junk), &k.public, None),
        Err(RuntimesError::Json(_))
    ));
}
