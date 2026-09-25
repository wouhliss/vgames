//! Cloud saves (A1-T13): blobs, compare-and-swap commits, quota and garbage collection.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use common::*;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use vgames_api::{
    AppState,
    jobs::{self, Registry},
    saves::blob_object,
    storage::BucketKind,
};

const ORIGIN: &str = "http://localhost:8080";

struct Env {
    state: AppState,
    app: Router,
    user: (Uuid, String),
    package: Uuid,
}

async fn env(pool: &PgPool, quota: Option<&str>) -> Env {
    let config = match quota {
        Some(q) => config_with(&[("VGAMES_SAVE_QUOTA_BYTES_PER_PACKAGE", q)]),
        None => test_config(),
    };
    let state = AppState::new(config, pool.clone()).unwrap();
    let app = vgames_api::http::router(state.clone());
    let (user_id, _, token) = seed_session(pool, "user").await;
    let package: Uuid = sqlx::query_scalar(
        "INSERT INTO packages (slug, title, created_by, status) VALUES ('game', 'Game', $1, 'published') RETURNING id",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .unwrap();
    Env {
        state,
        app,
        user: (user_id, token),
        package,
    }
}

fn blake3_hex(data: &[u8]) -> String {
    vgames_core::Digest::of(data).to_hex()
}

fn file(path: &str, data: &[u8]) -> Value {
    json!({ "root": "saves", "path": path, "size": data.len(), "blake3": blake3_hex(data), "mtime": "2026-09-24T10:00:00Z" })
}

async fn call(
    e: &Env,
    token: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (StatusCode, Value) {
    let b = Request::builder()
        .method(method)
        .uri(format!("/v1/saves/{}{path}", e.package))
        .header("authorization", format!("Bearer {token}"));
    let req = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(v).unwrap()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let resp = send(&e.app, req).await;
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Prepares and uploads `blobs`, returning how many upload targets the server asked for.
async fn upload(e: &Env, blobs: &[&[u8]]) -> usize {
    let refs: Vec<Value> = blobs
        .iter()
        .map(|b| json!({ "blake3": blake3_hex(b), "size": b.len() }))
        .collect();
    let (status, prep) = call(
        e,
        &e.user.1,
        "POST",
        "/blobs/prepare",
        Some(&json!({ "blobs": refs })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{prep}");
    let missing = prep["missing"].as_array().unwrap();
    for t in missing {
        let data = blobs
            .iter()
            .find(|b| blake3_hex(b) == t["blake3"].as_str().unwrap())
            .unwrap();
        let mut req = Request::builder()
            .method("PUT")
            .uri(t["url"].as_str().unwrap().strip_prefix(ORIGIN).unwrap());
        for (k, v) in t["headers"].as_object().unwrap() {
            req = req.header(k.as_str(), v.as_str().unwrap());
        }
        let resp = send(&e.app, req.body(Body::from(data.to_vec())).unwrap()).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }
    missing.len()
}

async fn commit(e: &Env, parent: Option<&str>, files: Vec<Value>) -> (StatusCode, Value) {
    let body = json!({ "parent_snapshot_id": parent, "platform": "linux", "files": files });
    call(e, &e.user.1, "POST", "/snapshots", Some(&body)).await
}

#[sqlx::test(migrations = "./migrations")]
async fn saves_round_trip_through_blobs_and_snapshots(pool: PgPool) {
    let e = env(&pool, None).await;
    let (a, b) = (b"slot one".as_slice(), b"settings".as_slice());
    assert_eq!(
        call(&e, &e.user.1, "GET", "/head", None).await.0,
        StatusCode::NOT_FOUND
    );

    // A blob that was prepared but never uploaded cannot be referenced.
    let refs = json!({ "blobs": [{ "blake3": blake3_hex(a), "size": a.len() }, { "blake3": blake3_hex(b), "size": b.len() }] });
    let (_, prep) = call(&e, &e.user.1, "POST", "/blobs/prepare", Some(&refs)).await;
    assert_eq!(prep["missing"].as_array().unwrap().len(), 2);
    let (status, body) = commit(&e, None, vec![file("slot1.sav", a)]).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("blob_not_uploaded"))
    );

    assert_eq!(upload(&e, &[a, b]).await, 2);
    let (status, first) = commit(
        &e,
        None,
        vec![file("slot1.sav", a), file("cfg/settings.ini", b)],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(first["file_count"], 2);
    let first_id = first["id"].as_str().unwrap();
    let (_, head) = call(&e, &e.user.1, "GET", "/head", None).await;
    assert_eq!(head["id"], first_id);
    assert_eq!(head["files"][0]["path"], "slot1.sav");

    // Known blobs are not uploaded again; the next snapshot builds on the head.
    let c = b"slot one, later".as_slice();
    assert_eq!(upload(&e, &[a, b, c]).await, 1);
    let (status, second) = commit(
        &e,
        Some(first_id),
        vec![file("slot1.sav", c), file("cfg/settings.ini", b)],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(second["parent_id"], first_id);

    let (_, page) = call(&e, &e.user.1, "GET", "/snapshots?limit=1", None).await;
    assert_eq!(page["items"][0]["id"], second["id"]);
    assert!(page["items"][0].get("files").is_none());
    let cursor = page["next_cursor"].as_str().unwrap();
    let (_, rest) = call(
        &e,
        &e.user.1,
        "GET",
        &format!("/snapshots?limit=1&cursor={cursor}"),
        None,
    )
    .await;
    assert_eq!(rest["items"][0]["id"], first_id);
    let (status, got) = call(
        &e,
        &e.user.1,
        "GET",
        &format!("/snapshots/{first_id}"),
        None,
    )
    .await;
    assert_eq!(
        (status, got["files"].as_array().unwrap().len()),
        (StatusCode::OK, 2)
    );

    // Download what was saved.
    let (status, urls) = call(
        &e,
        &e.user.1,
        "POST",
        "/blobs/download-urls",
        Some(&json!({ "blake3": [blake3_hex(c)] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{urls}");
    let url = urls["items"][0]["url"].as_str().unwrap();
    let resp = send(&e.app, get_req(url.strip_prefix(ORIGIN).unwrap())).await;
    assert_eq!(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        c
    );

    // Nobody else sees them.
    let (_, _, other) = seed_session(&pool, "user").await;
    assert_eq!(
        call(&e, &other, "GET", "/head", None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&e, &other, "GET", &format!("/snapshots/{first_id}"), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (status, _) = call(
        &e,
        &other,
        "POST",
        "/blobs/download-urls",
        Some(&json!({ "blake3": [blake3_hex(c)] })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Validation.
    let (status, _) = commit(
        &e,
        Some(second["id"].as_str().unwrap()),
        vec![file("../escape", a)],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &e,
        &e.user.1,
        "POST",
        "/snapshots",
        Some(&json!({ "platform": "linux", "files": [] })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "parent_snapshot_id must be present, even if null"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_commits_on_one_parent_have_one_winner(pool: PgPool) {
    let e = std::sync::Arc::new(env(&pool, None).await);
    let data = b"progress".as_slice();
    upload(&e, &[data]).await;

    for parent in [None, Some(())] {
        let parent_id = match parent {
            None => None,
            Some(()) => Some(
                call(&e, &e.user.1, "GET", "/head", None).await.1["id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            ),
        };
        let race = (0..2).map(|i| {
            let (e, parent_id) = (e.clone(), parent_id.clone());
            tokio::spawn(async move {
                commit(
                    &e,
                    parent_id.as_deref(),
                    vec![file(&format!("slot{i}.sav"), data)],
                )
                .await
            })
        });
        let mut results = Vec::new();
        for r in race {
            results.push(r.await.unwrap());
        }
        results.sort_by_key(|(s, _)| s.as_u16());
        let [(won, winner), (lost, conflict)] = <[_; 2]>::try_from(results).unwrap();
        assert_eq!(
            (won, lost),
            (StatusCode::CREATED, StatusCode::CONFLICT),
            "{conflict}"
        );
        assert_eq!(conflict["code"], "save_head_conflict");
        assert_eq!(
            conflict["detail"], winner["id"],
            "the conflict names the current head"
        );
    }
    let snapshots: i64 = sqlx::query_scalar("SELECT count(*) FROM save_snapshots")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(snapshots, 2, "losing commits are rolled back");
}

#[sqlx::test(migrations = "./migrations")]
async fn the_quota_is_enforced(pool: PgPool) {
    let e = env(&pool, Some("1048576")).await;
    let refs = json!({ "blobs": [{ "blake3": "ab".repeat(32), "size": 2 * 1024 * 1024 }] });
    let (status, body) = call(&e, &e.user.1, "POST", "/blobs/prepare", Some(&refs)).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("save_quota_exceeded"))
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn gc_keeps_recent_history_and_referenced_blobs(pool: PgPool) {
    let e = env(&pool, None).await;
    let keep = b"kept".as_slice();
    upload(&e, &[keep]).await;
    let mut head: Option<String> = None;
    for i in 0..22 {
        let (status, s) = commit(
            &e,
            head.as_deref(),
            vec![file(&format!("slot{i}.sav"), keep)],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        head = Some(s["id"].as_str().unwrap().to_string());
    }
    // An old orphan (prepared and uploaded long ago, never committed) and a fresh one.
    let (old, fresh) = (b"abandoned".as_slice(), b"in flight".as_slice());
    upload(&e, &[old, fresh]).await;
    sqlx::query("UPDATE save_blobs SET created_at = now() - interval '2 days' WHERE blake3 = decode($1, 'hex')")
        .bind(blake3_hex(old))
        .execute(&pool)
        .await
        .unwrap();

    let registry = Registry::standard();
    jobs::enqueue_now(&pool, "saves.gc", json!({}), jobs::Enqueue::default())
        .await
        .unwrap();
    while jobs::run_one(&e.state, &registry, "test").await.unwrap() {}

    let snapshots: i64 = sqlx::query_scalar("SELECT count(*) FROM save_snapshots")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(snapshots, 20);
    assert_eq!(
        call(&e, &e.user.1, "GET", "/head", None).await.1["id"],
        head.unwrap()
    );
    let blobs: Vec<String> =
        sqlx::query_scalar("SELECT encode(blake3, 'hex') FROM save_blobs ORDER BY 1")
            .fetch_all(&pool)
            .await
            .unwrap();
    let mut expected = vec![blake3_hex(keep), blake3_hex(fresh)];
    expected.sort();
    assert_eq!(blobs, expected);
    let storage = &e.state.storage;
    assert!(
        storage
            .head(BucketKind::Saves, &blob_object(e.user.0, &blake3_hex(old)))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .head(BucketKind::Saves, &blob_object(e.user.0, &blake3_hex(keep)))
            .await
            .unwrap()
            .is_some()
    );
}
