//! A real package, uploaded and finalized through the API (A1-T11, A1-T12 tests).
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use super::*;
use std::{io::Read, sync::Arc};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use vgames_api::AppState;
use vgames_core::{
    Context, Digest, Envelope, SecretKey, Timestamp,
    trust::{FORMAT, PublisherKey, Revocation, TrustBundle, sign_bundle},
};
use vgames_pack::{
    PACK_SIZE, PackSource, Plan,
    manifest::{self, Execution, VersionIdentity},
    scan::{self, FsReader},
};

pub const ORIGIN: &str = "http://localhost:8080";
pub const SERVER: &str = "01920000-0000-7000-8000-00000000abcd";

pub fn key(n: u8) -> SecretKey {
    SecretKey::from_seed(&[n; 32])
}

pub fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

pub fn publisher(k: &SecretKey, holder: Uuid, not_after: &str) -> PublisherKey {
    PublisherKey {
        key_id: k.public_key().key_id(),
        public_key: k.public_key(),
        holder_user_id: holder,
        label: format!("key-{}", k.public_key().key_id()),
        not_before: ts("2026-01-01T00:00:00Z"),
        not_after: ts(not_after),
    }
}

pub struct World {
    pub state: AppState,
    pub app: Router,
    pub owner: String,
    pub package: Uuid,
}

/// Root `key(1)`; the owner holds keys 2 (valid), 4 (expired), 5 (revoked) and 6 (valid);
/// another admin holds key 3.
pub async fn world(pool: &PgPool) -> World {
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

pub fn json_req(method: &str, uri: &str, token: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

/// A small game: a few files, packed with the real planner and encoder.
pub struct Built {
    pub manifest: Vec<u8>,
    pub packs: Vec<Vec<u8>>,
}

pub fn build(identity: &VersionIdentity) -> Built {
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

pub async fn storage_call(
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

pub fn headers(t: &Value) -> Vec<(String, String)> {
    t["headers"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect()
}

pub async fn upload_pack(w: &World, version: Uuid, index: usize, data: Vec<u8>) {
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

pub async fn upload_manifest(w: &World, version: Uuid, bytes: Vec<u8>) {
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
pub async fn new_version(w: &World, wrong_sequence: bool) -> (Uuid, VersionIdentity) {
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

pub fn envelope(signer: &SecretKey, bytes: &[u8]) -> Value {
    serde_json::from_slice(&Envelope::sign(signer, Context::Manifest, bytes).to_bytes()).unwrap()
}

pub async fn finalize(
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
pub async fn full_upload(w: &World, wrong_sequence: bool) -> (Uuid, Built) {
    let (version, identity) = new_version(w, wrong_sequence).await;
    let built = build(&identity);
    for (i, p) in built.packs.iter().enumerate() {
        upload_pack(w, version, i, p.clone()).await;
    }
    upload_manifest(w, version, built.manifest.clone()).await;
    (version, built)
}

pub fn code(body: &Value) -> &str {
    body["code"].as_str().unwrap_or_default()
}
