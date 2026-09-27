//! `vgames verify` (A5-T06) against a mock API and object storage, with the
//! manifest test vector of vgames-core: a release passes while its key is
//! trusted, is refused once the key is revoked, passes again once re-signed,
//! and is refused when the manifest bytes or the signer do not match.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::process::{Command, Output};

use serde_json::json;
use vgames_core::Digest;
use vgames_core::sign::{Context, Envelope, SecretKey};
use vgames_core::trust::{SignedBundle, sign_bundle};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SERVER_ID: &str = "01920000-0000-7000-8000-000000000000";
const HOLDER: &str = "0192aaaa-0000-7000-8000-000000000001";
const PACKAGE: &str = "0192a6f0-1c2d-7e3f-8a9b-0c1d2e3f4a5b";
/// The manifest vector's version: Windows, sequence 12.
const VERSION: &str = "0192a6f1-aaaa-7bbb-8ccc-dddddddddddd";
const MANIFEST: &[u8] = include_bytes!("../../vgames-core/tests/vectors/pack_manifest.json");

fn root() -> SecretKey {
    SecretKey::from_seed(&[1; 32])
}
fn old_key() -> SecretKey {
    SecretKey::from_seed(&[2; 32])
}
fn new_key() -> SecretKey {
    SecretKey::from_seed(&[3; 32])
}

fn bundle(version: u64, trusted: &[SecretKey], revoked: &[SecretKey]) -> SignedBundle {
    let doc = json!({
        "format": "vgames.trust/1", "server_id": SERVER_ID, "version": version,
        "issued_at": "2026-09-24T10:00:00Z", "expires_at": null,
        "root_key_id": root().public_key().key_id(),
        "publishers": trusted.iter().map(|k| {
            let pk = k.public_key();
            json!({ "key_id": pk.key_id(), "public_key": pk, "holder_user_id": HOLDER, "label": "alice",
                    "not_before": "2026-09-24T00:00:00Z", "not_after": "2028-09-24T00:00:00Z" })
        }).collect::<Vec<_>>(),
        "revoked": revoked.iter().map(|k| json!({ "key_id": k.public_key().key_id(),
            "revoked_at": "2026-09-25T08:00:00Z", "reason": "rotated" })).collect::<Vec<_>>(),
        "next_root": null
    });
    let bytes = serde_json::to_vec(&doc).unwrap();
    SignedBundle::new(&bytes, sign_bundle(&root(), &bytes))
}

/// Serves the API and storage for one release, signed by `signer`, with
/// `served` as the manifest bytes storage hands out.
async fn serve(bundle: &SignedBundle, signer: &SecretKey, served: &[u8]) -> MockServer {
    let server = MockServer::start().await;
    let envelope = Envelope::sign(signer, Context::Manifest, MANIFEST);
    let descriptor = json!({
        "package_id": PACKAGE, "version_id": VERSION, "platform": "windows-x86_64",
        "sequence": 12, "version_label": "1.4.0", "total_size": 1, "pack_count": 1,
        "manifest": { "url": format!("{}/objects/manifest.json?sig=secret", server.uri()),
                      "size": MANIFEST.len(), "blake3": Digest::of(MANIFEST).to_hex(),
                      "expires_at": "2030-01-01T00:00:00Z" },
        "signature": serde_json::to_value(&envelope).unwrap(),
        "published_at": "2026-09-25T09:00:00Z"
    });
    let info = json!({ "format": "vgames.server/1", "server_id": SERVER_ID, "name": "Test",
        "api_versions": ["v1"], "root_public_key": root().public_key().to_base64(),
        "root_key_fingerprint": root().public_key().fingerprint().to_string(), "features": [] });
    let package = json!({ "id": PACKAGE, "slug": "my-game", "title": "My Game", "platforms": [],
        "updated_at": "2026-09-25T09:00:00Z", "status": "published", "field_sources": {},
        "created_at": "2026-09-25T09:00:00Z", "created_by": { "id": HOLDER, "username": "alice" } });
    for (p, body) in [
        ("/.well-known/vgames.json".to_owned(), info),
        (
            "/v1/trust/bundle".to_owned(),
            serde_json::to_value(bundle).unwrap(),
        ),
        (format!("/v1/admin/packages/{PACKAGE}"), package),
        (
            format!("/v1/packages/{PACKAGE}/releases/windows-x86_64"),
            descriptor,
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/objects/manifest.json"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(served.to_vec()))
        .mount(&server)
        .await;
    server
}

async fn verify(server: &MockServer) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vgames"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("VGAMES_ACCESS_TOKEN", format!("vga_{}", "e".repeat(43)))
        .args([
            "verify",
            "--server",
            &server.uri(),
            "--package",
            PACKAGE,
            "--platform",
            "windows-x86_64",
            "--root",
            &root().public_key().to_base64(),
        ]);
    tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        o.status,
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

async fn refused(server: &MockServer, expected: &str) {
    let out = verify(server).await;
    assert!(!out.status.success(), "{}", text(&out));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(expected), "expected {expected:?} in: {err}");
    assert!(!err.contains("sig=secret"), "the signed URL leaked: {err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_release_verifies_until_its_key_is_revoked_and_again_once_re_signed() {
    let v1 = bundle(1, &[old_key()], &[]);
    let out = verify(&serve(&v1, &old_key(), MANIFEST).await).await;
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&old_key().public_key().key_id().to_string()),
        "{stdout}"
    );
    assert!(stdout.contains("bundle v1"), "{stdout}");

    let v2 = bundle(2, &[new_key()], &[old_key()]);
    refused(&serve(&v2, &old_key(), MANIFEST).await, "revoked").await;

    let out = verify(&serve(&v2, &new_key(), MANIFEST).await).await;
    assert!(out.status.success(), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("bundle v2"));
}

#[tokio::test(flavor = "multi_thread")]
async fn other_bytes_or_an_unknown_signer_are_refused() {
    let v1 = bundle(1, &[old_key()], &[]);
    let mut tampered = MANIFEST.to_vec();
    let last = tampered.len() - 2;
    tampered[last] ^= 1;
    refused(
        &serve(&v1, &old_key(), &tampered).await,
        "does not match the BLAKE3",
    )
    .await;
    refused(
        &serve(&v1, &old_key(), &MANIFEST[1..]).await,
        "bytes, the descriptor says",
    )
    .await;
    refused(
        &serve(&v1, &new_key(), MANIFEST).await,
        "launchers would refuse",
    )
    .await;
}
