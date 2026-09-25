//! Finalize and re-sign (A1-T11 part 2): a real package built by `vgames-pack`, uploaded
//! through the fs backend and signed with test publisher keys.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::{io::Read, sync::Arc};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use vgames_api::{AppState, storage::BucketKind, versions::signature_object};
use vgames_core::{
    Context, Digest, Envelope, SecretKey, Timestamp,
    trust::{FORMAT, PublisherKey, Revocation, TrustBundle, sign_bundle},
};
use vgames_pack::{
    PACK_SIZE, PackSource, Plan,
    manifest::{self, Execution, VersionIdentity},
    scan::{self, FsReader},
};

const ORIGIN: &str = "http://localhost:8080";
const SERVER: &str = "01920000-0000-7000-8000-00000000abcd";

fn key(n: u8) -> SecretKey {
    SecretKey::from_seed(&[n; 32])
}

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn publisher(k: &SecretKey, holder: Uuid, not_after: &str) -> PublisherKey {
    PublisherKey {
        key_id: k.public_key().key_id(),
        public_key: k.public_key(),
        holder_user_id: holder,
        label: format!("key-{}", k.public_key().key_id()),
        not_before: ts("2026-01-01T00:00:00Z"),
        not_after: ts(not_after),
    }
}

struct World {
    state: AppState,
    app: Router,
    owner: String,
    package: Uuid,
}

/// Root `key(1)`; the owner holds keys 2 (valid), 4 (expired), 5 (revoked) and 6 (valid);
/// another admin holds key 3.
async fn world(pool: &PgPool) -> World {
    let config = config_with(&[("VGAMES_ROOT_PUBLIC_KEY", &key(1).public_key().to_base64())]);
    let state = AppState::new(config, pool.clone()).unwrap();
    let app = vgames_api::http::router(state.clone());
    let (owner_id, _, owner) = seed_session(pool, "owner").await;
    let (other_id, _, _) = seed_session(pool, "admin").await;
    let bundle = TrustBundle {
        format: FORMAT.into(),
        server_id: SERVER.parse().unwrap(),
        version: 1,
        issued_at: ts("2026-09-01T00:00:00Z"),
        expires_at: None,
        root_key_id: key(1).public_key().key_id(),
        publishers: vec![
            publisher(&key(2), owner_id, "2030-01-01T00:00:00Z"),
            publisher(&key(3), other_id, "2030-01-01T00:00:00Z"),
            publisher(&key(4), owner_id, "2026-02-01T00:00:00Z"),
            publisher(&key(5), owner_id, "2030-01-01T00:00:00Z"),
            publisher(&key(6), owner_id, "2030-01-01T00:00:00Z"),
        ],
        revoked: vec![Revocation {
            key_id: key(5).public_key().key_id(),
            revoked_at: ts("2026-09-02T00:00:00Z"),
            reason: "lost".into(),
        }],
        next_root: None,
    };
    let bytes = bundle.to_bytes();
    let body = json!({ "bundle": STANDARD.encode(&bytes), "signature": sign_bundle(&key(1), &bytes).to_base64() });
    let resp = send(
        &app,
        json_req("POST", "/v1/admin/trust/bundles", &owner, &body),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = send(
        &app,
        json_req(
            "POST",
            "/v1/admin/packages",
            &owner,
            &json!({ "title": "Game", "fetch_metadata": false }),
        ),
    )
    .await;
    let package = body_json(resp).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    World {
        state,
        app,
        owner,
        package,
    }
}

fn json_req(method: &str, uri: &str, token: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

/// A small game: a few files, packed with the real planner and encoder.
struct Built {
    manifest: Vec<u8>,
    packs: Vec<Vec<u8>>,
}

fn build(identity: &VersionIdentity) -> Built {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    std::fs::write(dir.path().join("game.bin"), vec![0x5a; 300_000]).unwrap();
    std::fs::write(
        dir.path().join("data/level1.dat"),
        (0..200_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>(),
    )
    .unwrap();
    std::fs::write(dir.path().join("readme.txt"), b"hello").unwrap();
    let scanned = scan::scan(dir.path()).unwrap();
    let plan = Arc::new(Plan::new(scanned.files, scanned.directories).unwrap());
    let source = PackSource::new(
        plan.clone(),
        Arc::new(plan.raw_packing(PACK_SIZE).unwrap()),
        Arc::new(FsReader::new(dir.path())),
    );
    let packs = (0..source.packing.pack_count())
        .map(|i| {
            let mut out = Vec::new();
            source
                .open_pack(i, 0)
                .unwrap()
                .read_to_end(&mut out)
                .unwrap();
            out
        })
        .collect();
    let hashes = source.hashes.finish().unwrap();
    let manifest = manifest::build(
        identity,
        &Execution::default(),
        &plan,
        &source.packing,
        &hashes,
    )
    .unwrap();
    Built { manifest, packs }
}

async fn storage_call(
    app: &Router,
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Vec<u8>,
) -> StatusCode {
    let mut b = Request::builder()
        .method(method)
        .uri(url.strip_prefix(ORIGIN).unwrap_or(url));
    for (k, v) in headers {
        b = b.header(k.as_str(), v.as_str());
    }
    send(app, b.body(Body::from(body)).unwrap()).await.status()
}

fn headers(t: &Value) -> Vec<(String, String)> {
    t["headers"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect()
}

async fn upload_pack(w: &World, version: Uuid, index: usize, data: Vec<u8>) {
    let resp = send(
        &w.app,
        bearer_request(
            "POST",
            &format!("/v1/admin/versions/{version}/packs/{index}/upload-session"),
            &w.owner,
        ),
    )
    .await;
    let t = body_json(resp).await;
    let mut b = Request::builder()
        .method("POST")
        .uri(t["url"].as_str().unwrap().strip_prefix(ORIGIN).unwrap());
    for (k, v) in headers(&t) {
        b = b.header(k, v);
    }
    let start = send(&w.app, b.body(Body::empty()).unwrap()).await;
    assert_eq!(start.status(), StatusCode::CREATED);
    let session = start.headers()["location"].to_str().unwrap().to_string();
    let range = format!("bytes 0-{}/{}", data.len() - 1, data.len());
    assert_eq!(
        storage_call(
            &w.app,
            "PUT",
            &session,
            &[("content-range".into(), range)],
            data
        )
        .await,
        StatusCode::OK
    );
}

async fn upload_manifest(w: &World, version: Uuid, bytes: Vec<u8>) {
    let resp = send(
        &w.app,
        bearer_request(
            "POST",
            &format!("/v1/admin/versions/{version}/manifest-upload"),
            &w.owner,
        ),
    )
    .await;
    let t = body_json(resp).await;
    assert_eq!(
        storage_call(
            &w.app,
            "PUT",
            t["url"].as_str().unwrap(),
            &headers(&t),
            bytes
        )
        .await,
        StatusCode::OK
    );
}

/// A fresh version with its identity (optionally with a wrong sequence).
async fn new_version(w: &World, wrong_sequence: bool) -> (Uuid, VersionIdentity) {
    let body = json!({ "platform": "linux-x86_64", "version_label": "1.0" });
    let v = body_json(
        send(
            &w.app,
            json_req(
                "POST",
                &format!("/v1/admin/packages/{}/versions", w.package),
                &w.owner,
                &body,
            ),
        )
        .await,
    )
    .await;
    let id: Uuid = v["id"].as_str().unwrap().parse().unwrap();
    let sequence = v["sequence"].as_u64().unwrap() + if wrong_sequence { 100 } else { 0 };
    let identity = VersionIdentity {
        server_id: SERVER.parse().unwrap(),
        package_id: w.package,
        version_id: id,
        sequence,
        version_label: "1.0".into(),
        platform: "linux-x86_64".into(),
        created_at: 1_790_244_000,
    };
    (id, identity)
}

fn envelope(signer: &SecretKey, bytes: &[u8]) -> Value {
    serde_json::from_slice(&Envelope::sign(signer, Context::Manifest, bytes).to_bytes()).unwrap()
}

async fn finalize(
    w: &World,
    version: Uuid,
    manifest: &[u8],
    signature: Value,
) -> (StatusCode, Value) {
    let body = json!({
        "manifest_size": manifest.len(),
        "manifest_blake3": Digest::of(manifest).to_hex(),
        "signature": signature,
    });
    let resp = send(
        &w.app,
        json_req(
            "POST",
            &format!("/v1/admin/versions/{version}/finalize"),
            &w.owner,
            &body,
        ),
    )
    .await;
    let status = resp.status();
    (status, body_json(resp).await)
}

/// Uploads everything for a version and finalizes it with `signer`.
async fn full_upload(w: &World, wrong_sequence: bool) -> (Uuid, Built) {
    let (version, identity) = new_version(w, wrong_sequence).await;
    let built = build(&identity);
    for (i, p) in built.packs.iter().enumerate() {
        upload_pack(w, version, i, p.clone()).await;
    }
    upload_manifest(w, version, built.manifest.clone()).await;
    (version, built)
}

fn code(body: &Value) -> &str {
    body["code"].as_str().unwrap_or_default()
}

#[sqlx::test(migrations = "./migrations")]
async fn a_real_package_finalizes_and_can_be_re_signed(pool: PgPool) {
    let w = world(&pool).await;
    let (version, built) = full_upload(&w, false).await;

    let (status, v) = finalize(
        &w,
        version,
        &built.manifest,
        envelope(&key(2), &built.manifest),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["state"], "verifying");
    assert_eq!(v["pack_count"], built.packs.len());
    assert_eq!(v["file_count"], 3);
    assert_eq!(v["total_size"], 500_005);
    assert_eq!(
        v["publisher_key_id"],
        key(2).public_key().key_id().to_string()
    );
    assert!(v["finalized_at"].is_string());

    let packs: i64 = sqlx::query_scalar("SELECT count(*) FROM package_packs WHERE version_id = $1")
        .bind(version)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(packs as usize, built.packs.len());
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'version.verify'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(jobs, 1);
    assert!(
        w.state
            .storage
            .head(BucketKind::Packages, &signature_object(w.package, version))
            .await
            .unwrap()
            .is_some()
    );

    // Finalizing twice is a conflict.
    let (status, body) = finalize(
        &w,
        version,
        &built.manifest,
        envelope(&key(2), &built.manifest),
    )
    .await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::CONFLICT, "version_not_uploading")
    );

    // Re-sign with another of the owner's keys; the manifest bytes stay the same.
    let uri = format!("/v1/admin/versions/{version}/signature");
    let resp = send(
        &w.app,
        json_req(
            "POST",
            &uri,
            &w.owner,
            &json!({ "signature": envelope(&key(6), &built.manifest) }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        body_json(resp).await["publisher_key_id"],
        key(6).public_key().key_id().to_string()
    );
    let resp = send(
        &w.app,
        json_req(
            "POST",
            &uri,
            &w.owner,
            &json!({ "signature": envelope(&key(6), b"other bytes") }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(code(&body_json(resp).await), "manifest_hash_mismatch");
    let resp = send(
        &w.app,
        json_req(
            "POST",
            &uri,
            &w.owner,
            &json!({ "signature": envelope(&key(3), &built.manifest) }),
        ),
    )
    .await;
    assert_eq!(code(&body_json(resp).await), "publisher_key_not_yours");
}

#[sqlx::test(migrations = "./migrations")]
async fn every_failure_has_its_own_code(pool: PgPool) {
    let w = world(&pool).await;

    let (version, built) = full_upload(&w, false).await;
    let m = &built.manifest;
    let mut tampered = envelope(&key(2), m);
    let mut sig = STANDARD
        .decode(tampered["signature"].as_str().unwrap())
        .unwrap();
    sig[0] ^= 1;
    tampered["signature"] = json!(STANDARD.encode(sig));
    let mut wrong_format = envelope(&key(2), m);
    wrong_format["format"] = json!("vgames.sig/2");
    // Missing fields are a request-shape error, not a signature one.
    let (status, body) = finalize(&w, version, m, json!({ "format": "vgames.sig/1" })).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::BAD_REQUEST, "validation_failed")
    );
    for (signature, expected) in [
        (envelope(&key(9), m), "publisher_key_untrusted"),
        (envelope(&key(5), m), "publisher_key_untrusted"),
        (envelope(&key(3), m), "publisher_key_not_yours"),
        (envelope(&key(4), m), "publisher_key_expired"),
        (tampered, "signature_invalid"),
        (wrong_format, "signature_invalid"),
    ] {
        let (status, body) = finalize(&w, version, m, signature).await;
        assert_eq!(
            (status, code(&body)),
            (StatusCode::UNPROCESSABLE_ENTITY, expected),
            "{body}"
        );
    }
    // Declared hash differs from the uploaded bytes.
    let mut other = m.clone();
    other.push(b' ');
    let (status, body) = finalize(&w, version, &other, envelope(&key(2), &other)).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::UNPROCESSABLE_ENTITY, "manifest_hash_mismatch"),
        "{body}"
    );

    // The manifest is signed but not a valid manifest.
    let (version, _) = new_version(&w, false).await;
    upload_manifest(&w, version, b"{\"format\":\"nope\"}".to_vec()).await;
    let bad = b"{\"format\":\"nope\"}";
    let (status, body) = finalize(&w, version, bad, envelope(&key(2), bad)).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::UNPROCESSABLE_ENTITY, "manifest_invalid"),
        "{body}"
    );
    assert_eq!(body["errors"][0]["field"], "manifest");

    // The manifest names another sequence.
    let (version, built) = full_upload(&w, true).await;
    let (status, body) = finalize(
        &w,
        version,
        &built.manifest,
        envelope(&key(2), &built.manifest),
    )
    .await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::UNPROCESSABLE_ENTITY, "manifest_mismatch"),
        "{body}"
    );
    assert_eq!(body["errors"][0]["field"], "sequence");

    // A pack is missing, then present with the wrong size.
    let (version, identity) = new_version(&w, false).await;
    let built = build(&identity);
    upload_manifest(&w, version, built.manifest.clone()).await;
    let (status, body) = finalize(
        &w,
        version,
        &built.manifest,
        envelope(&key(2), &built.manifest),
    )
    .await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::UNPROCESSABLE_ENTITY, "pack_missing"),
        "{body}"
    );
    assert_eq!(body["errors"][0]["field"], "packs[0]");
    let mut short = built.packs[0].clone();
    short.pop();
    upload_pack(&w, version, 0, short).await;
    let (status, body) = finalize(
        &w,
        version,
        &built.manifest,
        envelope(&key(2), &built.manifest),
    )
    .await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::UNPROCESSABLE_ENTITY, "pack_size_mismatch"),
        "{body}"
    );

    // Nothing was recorded for failed attempts.
    let states: Vec<String> = sqlx::query_scalar("SELECT DISTINCT state FROM package_versions")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(states, ["uploading"]);
}
