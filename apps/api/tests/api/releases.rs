//! Server-side verification, publish and yank (A1-T12 part 1).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use crate::common::{publishing::*, *};
use axum::http::StatusCode;
use bytes::Bytes;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use vgames_api::{
    jobs::{self, Registry},
    storage::BucketKind,
    versions::pack_object,
};

/// Uploads, finalizes (signed with the owner's key) and returns the version id.
async fn finalized(w: &World) -> (Uuid, Built) {
    let (version, built) = full_upload(w, false).await;
    let (status, body) = finalize(
        w,
        version,
        &built.manifest,
        envelope(&key(2), &built.manifest),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    (version, built)
}

async fn run_jobs(w: &World) {
    let registry = Registry::standard();
    while jobs::run_one(&w.state, &registry, "test").await.unwrap() {}
}

async fn get(w: &World, version: Uuid) -> Value {
    body_json(
        send(
            &w.app,
            bearer_request("GET", &format!("/v1/admin/versions/{version}"), &w.owner),
        )
        .await,
    )
    .await
}

async fn post(w: &World, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = match body {
        Some(b) => json_req("POST", uri, &w.owner, &b),
        None => bearer_request("POST", uri, &w.owner),
    };
    let resp = send(&w.app, req).await;
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn release_of(pool: &PgPool, package: Uuid) -> Option<Uuid> {
    sqlx::query_scalar("SELECT version_id FROM package_releases WHERE package_id = $1 AND platform = 'linux-x86_64'")
        .bind(package)
        .fetch_optional(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn verified_versions_publish_and_yank_falls_back(pool: PgPool) {
    let w = world(&pool).await;

    // v1 verifies, publishes and becomes the release.
    let (v1, _) = finalized(&w).await;
    let (status, body) = post(&w, &format!("/v1/admin/versions/{v1}/publish"), None).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("version_not_ready")),
        "not verified yet"
    );
    run_jobs(&w).await;
    let v = get(&w, v1).await;
    assert_eq!(v["state"], "ready", "{v}");
    assert_eq!(v["verify_progress"], 1.0);
    assert!(v["verified_at"].is_string());
    let (status, v) = post(&w, &format!("/v1/admin/versions/{v1}/publish"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["state"], "published");
    assert_eq!(v["is_current_release"], true);
    assert_eq!(release_of(&pool, w.package).await, Some(v1));

    // v2 replaces it; v1 stays published (installable) but is no longer current.
    let (v2, _) = finalized(&w).await;
    run_jobs(&w).await;
    assert_eq!(
        post(&w, &format!("/v1/admin/versions/{v2}/publish"), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(release_of(&pool, w.package).await, Some(v2));
    assert_eq!(get(&w, v1).await["is_current_release"], false);

    // Yanking the current release falls back to the newest remaining published version.
    let uri = format!("/v1/admin/versions/{v2}/yank");
    assert_eq!(
        post(&w, &uri, Some(json!({ "reason": "no" }))).await.0,
        StatusCode::BAD_REQUEST
    );
    let (status, v) = post(&w, &uri, Some(json!({ "reason": "crashes on start" }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["state"], "yanked");
    assert!(v["yanked_at"].is_string());
    assert_eq!(release_of(&pool, w.package).await, Some(v1));
    let (status, body) = post(&w, &uri, Some(json!({ "reason": "again" }))).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("version_not_published"))
    );
    assert_eq!(
        post(&w, &format!("/v1/admin/versions/{v2}/publish"), None)
            .await
            .0,
        StatusCode::CONFLICT
    );

    // Yanking the last published version leaves no release.
    assert_eq!(
        post(
            &w,
            &format!("/v1/admin/versions/{v1}/yank"),
            Some(json!({ "reason": "old" }))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(release_of(&pool, w.package).await, None);
    let audit: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action IN ('version.publish', 'version.yank')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audit, 4);
}

#[sqlx::test(migrations = "./migrations")]
async fn a_corrupted_byte_fails_verification_at_its_chunk(pool: PgPool) {
    let w = world(&pool).await;
    let (version, built) = finalized(&w).await;

    // Flip one byte in the middle of pack 0 in storage (same size, so finalize had passed).
    let mut pack = built.packs[0].clone();
    let at = pack.len() / 2;
    pack[at] ^= 0xff;
    w.state
        .storage
        .put_small(
            BucketKind::Packages,
            &pack_object(w.package, version, 0),
            Bytes::from(pack),
            "application/octet-stream",
        )
        .await
        .unwrap();
    run_jobs(&w).await;

    let v = get(&w, version).await;
    assert_eq!(v["state"], "failed");
    let reason = v["failure_reason"].as_str().unwrap();
    assert!(reason.starts_with("pack 0: chunk "), "{reason}");
    let (status, body) = post(&w, &format!("/v1/admin/versions/{version}/publish"), None).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("version_not_ready"))
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn aborting_during_verification_wins(pool: PgPool) {
    let w = world(&pool).await;
    let (version, _) = finalized(&w).await;
    let resp = send(
        &w.app,
        bearer_request("DELETE", &format!("/v1/admin/versions/{version}"), &w.owner),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    run_jobs(&w).await;
    assert_eq!(get(&w, version).await["state"], "aborted");
}
