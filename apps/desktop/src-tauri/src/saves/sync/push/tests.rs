use serde_json::json;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{body_bytes, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::super::tests::{Rig, snapshot};
use super::*;

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

async fn push(rig: &Rig) -> Result<Push, Error> {
    rig.sync
        .push(rig.scope, rig.locations(), CancellationToken::new())
        .await
}

/// `prepare` answers that `missing` must be uploaded to `/storage/<hash>`.
async fn serve_prepare(server: &MockServer, rig: &Rig, missing: &[&[u8]], times: u64) {
    let missing: Vec<_> = missing
        .iter()
        .map(|bytes| {
            let hash = hash(bytes);
            json!({ "blake3": hash, "url": format!("{}/storage/{hash}", server.uri()),
                "method": "PUT", "headers": {}, "expires_at": "2099-01-01T00:00:00Z" })
        })
        .collect();
    Mock::given(method("POST"))
        .and(path(format!(
            "/v1/saves/{}/blobs/prepare",
            rig.scope.package.package_id
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "missing": missing })))
        .expect(times)
        .mount(server)
        .await;
}

async fn serve_upload(server: &MockServer, bytes: &[u8], times: u64) {
    Mock::given(method("PUT"))
        .and(path(format!("/storage/{}", hash(bytes))))
        .and(body_bytes(bytes.to_vec()))
        .respond_with(ResponseTemplate::new(200))
        .expect(times)
        .mount(server)
        .await;
}

fn commit_route(rig: &Rig) -> String {
    format!("/v1/saves/{}/snapshots", rig.scope.package.package_id)
}

async fn commit_requests(server: &MockServer, rig: &Rig) -> Vec<wiremock::Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| {
            request.method.as_str() == "POST" && request.url.path() == commit_route(rig)
        })
        .collect()
}

#[tokio::test]
async fn new_progress_is_uploaded_committed_and_becomes_the_base() {
    let server = MockServer::start().await;
    let rig = Rig::new(&server.uri()).await;
    rig.write("slot.sav", b"progress");
    let pushed = Uuid::now_v7();
    serve_prepare(&server, &rig, &[b"progress"], 1).await;
    serve_upload(&server, b"progress", 1).await;
    Mock::given(method("POST"))
        .and(path(commit_route(&rig)))
        .respond_with(ResponseTemplate::new(201).set_body_json(snapshot(
            pushed,
            rig.scope.package.package_id,
            &[("slot.sav", b"progress")],
        )))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(push(&rig).await.unwrap(), Push::Pushed { snapshot: pushed });
    let commit = &commit_requests(&server, &rig).await[0];
    let body: serde_json::Value = serde_json::from_slice(&commit.body).unwrap();
    assert_eq!(body["parent_snapshot_id"], serde_json::Value::Null);
    assert_eq!(body["files"][0]["path"], "slot.sav");
    assert!(commit.headers.get("idempotency-key").is_some());
    let state = rig.store.state(rig.scope).await.unwrap();
    assert_eq!(state.base_snapshot_id, Some(pushed));
    assert!(!state.pending && state.push.is_none());
    // Nothing changed since: no further request (the mocks expect one each).
    assert_eq!(push(&rig).await.unwrap(), Push::Unchanged);
}

#[tokio::test]
async fn a_conflict_keeps_the_saves_and_the_retry_identity() {
    let server = MockServer::start().await;
    let rig = Rig::new(&server.uri()).await;
    let base = Uuid::now_v7();
    rig.write("slot.sav", b"synced");
    rig.synced(base, &[("slot.sav", b"synced")]).await;
    rig.write("slot.sav", b"played on");
    serve_prepare(&server, &rig, &[], 2).await;
    Mock::given(method("POST"))
        .and(path(commit_route(&rig)))
        .respond_with(
            ResponseTemplate::new(409).set_body_json(json!({"code": "save_head_conflict"})),
        )
        .expect(2)
        .mount(&server)
        .await;
    assert_eq!(push(&rig).await.unwrap(), Push::Conflict);
    assert_eq!(push(&rig).await.unwrap(), Push::Conflict);
    let commits = commit_requests(&server, &rig).await;
    let keys: Vec<_> = commits
        .iter()
        .map(|request| request.headers.get("idempotency-key").unwrap().clone())
        .collect();
    assert_eq!(
        keys[0], keys[1],
        "an identical retry keeps its operation id"
    );
    let body: serde_json::Value = serde_json::from_slice(&commits[0].body).unwrap();
    assert_eq!(body["parent_snapshot_id"], json!(base));
    let state = rig.store.state(rig.scope).await.unwrap();
    assert!(state.pending);
    assert_eq!(state.base_snapshot_id, Some(base));
    assert_eq!(rig.read("slot.sav").unwrap(), b"played on");
}

#[tokio::test]
async fn a_full_quota_uploads_nothing_and_stays_pending() {
    let server = MockServer::start().await;
    let rig = Rig::new(&server.uri()).await;
    rig.write("slot.sav", b"too big");
    Mock::given(method("POST"))
        .and(path(format!(
            "/v1/saves/{}/blobs/prepare",
            rig.scope.package.package_id
        )))
        .respond_with(
            ResponseTemplate::new(413).set_body_json(json!({"code": "save_quota_exceeded"})),
        )
        .mount(&server)
        .await;
    serve_upload(&server, b"too big", 0).await;
    assert_eq!(push(&rig).await.unwrap(), Push::QuotaExceeded);
    assert!(commit_requests(&server, &rig).await.is_empty());
    assert!(rig.store.state(rig.scope).await.unwrap().pending);
    assert_eq!(rig.read("slot.sav").unwrap(), b"too big");
}

#[tokio::test]
async fn a_blob_the_server_never_received_is_uploaded_again_once() {
    let server = MockServer::start().await;
    let rig = Rig::new(&server.uri()).await;
    rig.write("slot.sav", b"retry me");
    let pushed = Uuid::now_v7();
    serve_prepare(&server, &rig, &[b"retry me"], 2).await;
    serve_upload(&server, b"retry me", 2).await;
    Mock::given(method("POST"))
        .and(path(commit_route(&rig)))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({"code": "blob_not_uploaded"})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(commit_route(&rig)))
        .respond_with(ResponseTemplate::new(201).set_body_json(snapshot(
            pushed,
            rig.scope.package.package_id,
            &[("slot.sav", b"retry me")],
        )))
        .mount(&server)
        .await;
    assert_eq!(push(&rig).await.unwrap(), Push::Pushed { snapshot: pushed });
}

#[tokio::test]
async fn an_unreachable_server_keeps_the_push_pending() {
    let rig = Rig::new("http://127.0.0.1:1").await;
    rig.write("slot.sav", b"offline progress");
    assert_eq!(push(&rig).await.unwrap(), Push::Offline);
    let state = rig.store.state(rig.scope).await.unwrap();
    assert!(state.pending && state.push.is_some());
}

#[tokio::test]
async fn synced_saves_send_no_request() {
    let server = MockServer::start().await;
    let rig = Rig::new(&server.uri()).await;
    rig.write("slot.sav", b"synced");
    rig.synced(Uuid::now_v7(), &[("slot.sav", b"synced")]).await;
    assert_eq!(push(&rig).await.unwrap(), Push::Unchanged);
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// Answers `prepare` like the server, but first rewrites the save on disk,
/// as a game still flushing after the scan would.
struct RewriteThenPrepare {
    file: std::path::PathBuf,
    upload: String,
    blake3: String,
}

impl wiremock::Respond for RewriteThenPrepare {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        std::fs::write(&self.file, b"written after the scan").unwrap();
        ResponseTemplate::new(200).set_body_json(json!({ "missing": [{
            "blake3": self.blake3, "url": self.upload, "method": "PUT", "headers": {},
            "expires_at": "2099-01-01T00:00:00Z" }] }))
    }
}

#[tokio::test]
async fn a_save_rewritten_after_the_scan_is_never_uploaded_under_the_old_hash() {
    let server = MockServer::start().await;
    let rig = Rig::new(&server.uri()).await;
    rig.write("slot.sav", b"scanned bytes");
    Mock::given(method("POST"))
        .and(path(format!(
            "/v1/saves/{}/blobs/prepare",
            rig.scope.package.package_id
        )))
        .respond_with(RewriteThenPrepare {
            file: rig.game().join("slot.sav"),
            upload: format!("{}/storage/upload", server.uri()),
            blake3: hash(b"scanned bytes"),
        })
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    assert!(matches!(
        push(&rig).await,
        Err(Error::Files(scan::Error::Changed))
    ));
    assert!(commit_requests(&server, &rig).await.is_empty());
    let state = rig.store.state(rig.scope).await.unwrap();
    assert!(state.pending && state.base_snapshot_id.is_none());
    assert_eq!(rig.read("slot.sav").unwrap(), b"written after the scan");
}
