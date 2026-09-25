//! Cloud saves (A1-T13, 06-cloud-saves §4): blobs, snapshots, compare-and-swap head,
//! quota, garbage collection.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use crate::common;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use common::{publishing::ORIGIN, *};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use vgames_api::{AppState, saves};

const KIB: usize = 1024;
/// The smallest quota the configuration accepts: 1 MiB.
const QUOTA: &str = "1048576";

struct Env {
    state: AppState,
    app: Router,
    token: String,
    user: Uuid,
    package: Uuid,
}

async fn env_with(pool: PgPool, overrides: &[(&'static str, &str)]) -> Env {
    let state = AppState::new(config_with(overrides), pool.clone()).unwrap();
    let app = vgames_api::http::router(state.clone());
    let (user, _, token) = seed_session(&pool, "user").await;
    let package = new_package(&pool, user).await;
    Env {
        state,
        app,
        token,
        user,
        package,
    }
}

async fn env(pool: PgPool) -> Env {
    env_with(pool, &[]).await
}

async fn new_package(pool: &PgPool, created_by: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO packages (slug, title, created_by) VALUES ('saves-game', 'Saves Game', $1) RETURNING id",
    )
    .bind(created_by)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn req(method: &str, uri: &str, token: &str, body: Option<&Value>) -> Request<Body> {
    let b = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(v).unwrap()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

async fn call(e: &Env, method: &str, uri: &str, body: Option<&Value>) -> (StatusCode, Value) {
    let resp = send(&e.app, req(method, uri, &e.token, body)).await;
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, v)
}

fn hash(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

fn file(root: &str, path: &str, data: &[u8]) -> Value {
    json!({ "root": root, "path": path, "size": data.len(), "blake3": hash(data), "mtime": "2026-09-25T10:00:00Z" })
}

/// Prepares and uploads `blobs`; returns how many the server asked for.
async fn push_blobs(e: &Env, blobs: &[&[u8]]) -> usize {
    let refs: Vec<Value> = blobs
        .iter()
        .map(|d| json!({ "blake3": hash(d), "size": d.len() }))
        .collect();
    let (status, body) = call(
        e,
        "POST",
        &format!("/v1/saves/{}/blobs/prepare", e.package),
        Some(&json!({ "blobs": refs })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let missing = body["missing"].as_array().unwrap();
    for t in missing {
        assert_eq!(t["method"], "PUT");
        let data = blobs
            .iter()
            .find(|d| hash(d) == t["blake3"].as_str().unwrap())
            .unwrap();
        let mut b = Request::builder()
            .method("PUT")
            .uri(t["url"].as_str().unwrap().strip_prefix(ORIGIN).unwrap());
        for (k, v) in t["headers"].as_object().unwrap() {
            b = b.header(k.as_str(), v.as_str().unwrap());
        }
        let resp = send(&e.app, b.body(Body::from(data.to_vec())).unwrap()).await;
        assert_eq!(resp.status(), StatusCode::OK, "upload {}", t["blake3"]);
    }
    missing.len()
}

async fn commit(e: &Env, parent: Option<&str>, files: Vec<Value>) -> (StatusCode, Value) {
    call(
        e,
        "POST",
        &format!("/v1/saves/{}/snapshots", e.package),
        Some(&json!({ "parent_snapshot_id": parent, "platform": "linux", "files": files })),
    )
    .await
}

/// Pushes one file per blob and commits; returns the new snapshot id.
async fn save(e: &Env, parent: Option<&str>, blobs: &[&[u8]]) -> String {
    push_blobs(e, blobs).await;
    let files = blobs
        .iter()
        .enumerate()
        .map(|(i, d)| file("saves", &format!("slot{i}.sav"), d))
        .collect();
    let (status, body) = commit(e, parent, files).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

#[sqlx::test(migrations = "./migrations")]
async fn push_restore_round_trip(pool: PgPool) {
    let e = env(pool).await;
    let p = e.package;

    let (status, body) = call(&e, "GET", &format!("/v1/saves/{p}/head"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "no_saves");

    let a: &[u8] = b"level 3, 42 coins";
    let empty: &[u8] = b"";
    assert_eq!(push_blobs(&e, &[a, empty]).await, 2);
    // Uploaded blobs are not asked for again (only confirmed ones are known, so the
    // first commit confirms them; before that they are still pending).
    let files = vec![
        file("saves", "slot1.sav", a),
        file("saves", "cfg/empty.ini", empty),
        file("config", "slot1-copy.sav", a),
    ];
    let (status, first) = commit(&e, None, files.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(first["file_count"], 3);
    assert_eq!(first["total_size"], 2 * a.len());
    assert_eq!(first["platform"], "linux");
    assert!(first.get("parent_id").is_none());
    assert_eq!(first["files"], json!(files));
    assert_eq!(push_blobs(&e, &[a, empty]).await, 0);

    let (status, head) = call(&e, "GET", &format!("/v1/saves/{p}/head"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(head, first);

    let first_id = first["id"].as_str().unwrap();
    let b: &[u8] = b"level 4, 50 coins";
    let second_id = save(&e, Some(first_id), &[b]).await;

    // History, newest first, paginated.
    let (status, page) = call(&e, "GET", &format!("/v1/saves/{p}/snapshots?limit=1"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["items"][0]["id"], second_id);
    assert_eq!(page["items"][0]["parent_id"], first_id);
    assert!(page["items"][0].get("files").is_none());
    let cursor = page["next_cursor"].as_str().unwrap();
    let (_, page2) = call(
        &e,
        "GET",
        &format!("/v1/saves/{p}/snapshots?limit=1&cursor={cursor}"),
        None,
    )
    .await;
    assert_eq!(page2["items"][0]["id"], first_id);
    assert!(page2.get("next_cursor").is_none());

    let (status, snap) = call(
        &e,
        "GET",
        &format!("/v1/saves/{p}/snapshots/{first_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(snap["files"], json!(files));

    // Restore: signed GETs return the exact bytes.
    let (status, urls) = call(
        &e,
        "POST",
        &format!("/v1/saves/{p}/blobs/download-urls"),
        Some(&json!({ "blake3": [hash(a), hash(empty)] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{urls}");
    for item in urls["items"].as_array().unwrap() {
        let resp = send(
            &e.app,
            get_req(item["url"].as_str().unwrap().strip_prefix(ORIGIN).unwrap()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(hash(&bytes), item["blake3"].as_str().unwrap());
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_commits_on_one_parent_one_wins(pool: PgPool) {
    let e = env(pool).await;
    let a: &[u8] = b"device A progress";
    let b: &[u8] = b"device B progress";
    push_blobs(&e, &[a, b]).await;

    // Both devices start from "no saves yet".
    let (r1, r2) = tokio::join!(
        commit(&e, None, vec![file("s", "save.dat", a)]),
        commit(&e, None, vec![file("s", "save.dat", b)]),
    );
    let mut results = [r1, r2];
    results.sort_by_key(|(s, _)| s.as_u16());
    assert_eq!(results[0].0, StatusCode::CREATED, "{}", results[0].1);
    assert_eq!(results[1].0, StatusCode::CONFLICT, "{}", results[1].1);
    let winner = results[0].1["id"].as_str().unwrap().to_string();
    assert_eq!(results[1].1["code"], "save_head_conflict");
    assert!(
        results[1].1["detail"].as_str().unwrap().contains(&winner),
        "the 409 names the current head"
    );

    // Both devices continue from the same parent: exactly one wins again.
    let (r1, r2) = tokio::join!(
        commit(&e, Some(&winner), vec![file("s", "save.dat", a)]),
        commit(&e, Some(&winner), vec![file("s", "save.dat", b)]),
    );
    let mut results = [r1, r2];
    results.sort_by_key(|(s, _)| s.as_u16());
    assert_eq!(results[0].0, StatusCode::CREATED, "{}", results[0].1);
    assert_eq!(results[1].0, StatusCode::CONFLICT, "{}", results[1].1);
    let new_head = results[0].1["id"].as_str().unwrap();
    assert!(results[1].1["detail"].as_str().unwrap().contains(new_head));
    assert_eq!(results[1].1["errors"][0]["field"], "parent_snapshot_id");

    let (_, head) = call(&e, "GET", &format!("/v1/saves/{}/head", e.package), None).await;
    assert_eq!(head["id"], new_head);
    // A stale parent is refused too, and nothing was stored for the losers.
    let (status, body) = commit(&e, Some(&winner), vec![file("s", "save.dat", a)]).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (_, page) = call(
        &e,
        "GET",
        &format!("/v1/saves/{}/snapshots", e.package),
        None,
    )
    .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
}

#[sqlx::test(migrations = "./migrations")]
async fn quota_is_enforced_with_413(pool: PgPool) {
    let e = env_with(pool, &[("VGAMES_SAVE_QUOTA_BYTES_PER_PACKAGE", QUOTA)]).await;
    let big = vec![1u8; 700 * KIB];
    let other = vec![2u8; 400 * KIB];

    // One push larger than the quota.
    let (status, body) = call(
        &e,
        "POST",
        &format!("/v1/saves/{}/blobs/prepare", e.package),
        Some(&json!({ "blobs": [
            { "blake3": hash(&big), "size": big.len() },
            { "blake3": hash(&other), "size": other.len() }
        ] })),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(body["code"], "save_quota_exceeded");

    // Uploads still in flight count: 700 KiB pending + 400 KiB more is over the limit.
    push_blobs(&e, &[&big]).await;
    let (status, body) = call(
        &e,
        "POST",
        &format!("/v1/saves/{}/blobs/prepare", e.package),
        Some(&json!({ "blobs": [{ "blake3": hash(&other), "size": other.len() }] })),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");

    // A snapshot whose distinct blobs exceed the quota is refused at commit too.
    let (status, body) = commit(&e, None, vec![file("s", "a", &big), file("s", "b", &other)]).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(body["code"], "save_quota_exceeded");

    // The same content in many files counts once.
    let (status, body) = commit(&e, None, vec![file("s", "a", &big), file("s", "b", &big)]).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[sqlx::test(migrations = "./migrations")]
async fn old_history_is_dropped_to_fit_the_quota(pool: PgPool) {
    let e = env_with(pool, &[("VGAMES_SAVE_QUOTA_BYTES_PER_PACKAGE", QUOTA)]).await;
    let (a, b, c) = (
        vec![1u8; 400 * KIB],
        vec![2u8; 400 * KIB],
        vec![3u8; 400 * KIB],
    );
    let s1 = save(&e, None, &[&a]).await;
    let s2 = save(&e, Some(&s1), &[&b]).await;
    // a + b + c = 1200 KiB > 1024 KiB: the oldest snapshot goes, the new head and s2 stay.
    let s3 = save(&e, Some(&s2), &[&c]).await;
    let (_, page) = call(
        &e,
        "GET",
        &format!("/v1/saves/{}/snapshots", e.package),
        None,
    )
    .await;
    let ids: Vec<&str> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![s3.as_str(), s2.as_str()]);
    let (status, _) = call(
        &e,
        "GET",
        &format!("/v1/saves/{}/snapshots/{s1}", e.package),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "./migrations")]
async fn blobs_must_be_uploaded_before_commit(pool: PgPool) {
    let e = env(pool.clone()).await;
    let data: &[u8] = b"never uploaded";

    // Referenced but never prepared.
    let (status, body) = commit(&e, None, vec![file("s", "x.sav", data)]).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "blob_not_uploaded");

    // Prepared but the PUT never happened.
    let (status, body) = call(
        &e,
        "POST",
        &format!("/v1/saves/{}/blobs/prepare", e.package),
        Some(&json!({ "blobs": [{ "blake3": hash(data), "size": data.len() }] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["missing"].as_array().unwrap().len(), 1);
    let (status, body) = commit(&e, None, vec![file("s", "x.sav", data)]).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "blob_not_uploaded");
    let uploaded: Option<time::OffsetDateTime> =
        sqlx::query_scalar("SELECT uploaded_at FROM save_blobs WHERE user_id = $1")
            .bind(e.user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(uploaded.is_none());

    // Once uploaded, the commit confirms it through storage metadata.
    push_blobs(&e, &[data]).await;
    let (status, body) = commit(&e, None, vec![file("s", "x.sav", data)]).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // A file claiming another size for the same content is refused.
    let mut wrong = file("s", "y.sav", data);
    wrong["size"] = json!(data.len() + 1);
    let (status, body) = commit(&e, Some(body["id"].as_str().unwrap()), vec![wrong]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

async fn blob_rows(pool: &PgPool) -> Vec<String> {
    let rows: Vec<Vec<u8>> = sqlx::query_scalar("SELECT blake3 FROM save_blobs ORDER BY blake3")
        .fetch_all(pool)
        .await
        .unwrap();
    rows.into_iter().map(hex::encode).collect()
}

#[sqlx::test(migrations = "./migrations")]
async fn gc_removes_unreferenced_blobs_only(pool: PgPool) {
    let e = env(pool.clone()).await;
    let kept: &[u8] = b"referenced by the head";
    let dropped: &[u8] = b"uploaded, never committed";
    let fresh: &[u8] = b"uploaded a moment ago";
    save(&e, None, &[kept]).await;
    push_blobs(&e, &[dropped, fresh]).await;
    // Everything but `fresh` was last touched two days ago.
    sqlx::query(
        "UPDATE save_blobs SET created_at = now() - interval '2 days',
                uploaded_at = CASE WHEN uploaded_at IS NULL THEN NULL ELSE now() - interval '2 days' END
         WHERE blake3 <> $1",
    )
    .bind(blake3::hash(fresh).as_bytes().to_vec())
    .execute(&pool)
    .await
    .unwrap();

    let report = saves::gc(&e.state).await.unwrap();
    assert_eq!(report.blobs, 1);
    let mut expect = vec![hash(kept), hash(fresh)];
    expect.sort();
    assert_eq!(blob_rows(&pool).await, expect);

    let objects = e
        .state
        .storage
        .list_prefix(
            vgames_api::storage::BucketKind::Saves,
            &format!("v1/{}/", e.user),
        )
        .await
        .unwrap();
    let mut expect_objects: Vec<String> = [kept, fresh]
        .iter()
        .map(|d| saves::blob_object(e.user, &hash(d)))
        .collect();
    expect_objects.sort();
    assert_eq!(objects, expect_objects);

    // Nothing left to collect; the head still restores.
    assert_eq!(saves::gc(&e.state).await.unwrap().blobs, 0);
    let (status, _) = call(
        &e,
        "POST",
        &format!("/v1/saves/{}/blobs/download-urls", e.package),
        Some(&json!({ "blake3": [hash(kept)] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test(migrations = "./migrations")]
async fn gc_keeps_the_newest_snapshots_and_the_head(pool: PgPool) {
    let e = env(pool.clone()).await;
    let mut parent: Option<String> = None;
    let mut ids = Vec::new();
    for i in 0..23u8 {
        let data = vec![i; 16];
        let id = save(&e, parent.as_deref(), &[&data]).await;
        parent = Some(id.clone());
        ids.push(id);
    }
    sqlx::query("UPDATE save_blobs SET created_at = now() - interval '2 days', uploaded_at = now() - interval '2 days'")
        .execute(&pool)
        .await
        .unwrap();
    let report = saves::gc(&e.state).await.unwrap();
    assert_eq!(report.snapshots, 3);
    assert_eq!(report.blobs, 3, "blobs of dropped snapshots are collected");
    let left: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM save_snapshots ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    let expect: Vec<Uuid> = ids[3..].iter().map(|s| s.parse().unwrap()).collect();
    assert_eq!(left, expect);
    let (_, head) = call(&e, "GET", &format!("/v1/saves/{}/head", e.package), None).await;
    assert_eq!(head["id"], ids[22]);
}

#[sqlx::test(migrations = "./migrations")]
async fn users_only_see_their_own_saves(pool: PgPool) {
    let e = env(pool.clone()).await;
    let data: &[u8] = b"mine";
    let id = save(&e, None, &[data]).await;
    let (_, _, other_token) = seed_session(&pool, "admin").await;
    let other = Env {
        token: other_token,
        ..e
    };
    let p = other.package;
    let (status, body) = call(&other, "GET", &format!("/v1/saves/{p}/head"), None).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("no_saves"))
    );
    let (status, _) = call(
        &other,
        "GET",
        &format!("/v1/saves/{p}/snapshots/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, page) = call(&other, "GET", &format!("/v1/saves/{p}/snapshots"), None).await;
    assert_eq!(page["items"], json!([]));
    let (status, _) = call(
        &other,
        "POST",
        &format!("/v1/saves/{p}/blobs/download-urls"),
        Some(&json!({ "blake3": [hash(data)] })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Their own upload of the same content is a separate blob they must upload.
    assert_eq!(push_blobs(&other, &[data]).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn requests_are_validated(pool: PgPool) {
    let e = env(pool.clone()).await;
    let p = e.package;
    let data: &[u8] = b"x";
    let cases = [
        (
            json!({ "parent_snapshot_id": null, "platform": "linux", "files": [], "device_id": Uuid::now_v7() }),
            "unknown_field",
            "device_id",
        ),
        (
            json!({ "platform": "linux", "files": [] }),
            "validation_failed",
            "parent_snapshot_id",
        ),
        (
            json!({ "parent_snapshot_id": null, "platform": "linux", "files": [file("s", "../escape", data)] }),
            "validation_failed",
            "files[0].path",
        ),
        (
            json!({ "parent_snapshot_id": null, "platform": "linux", "files": [file("Bad Root", "a", data)] }),
            "validation_failed",
            "files[0].root",
        ),
        (
            json!({ "parent_snapshot_id": null, "platform": "linux", "files": [file("s", "A.sav", data), file("s", "a.sav", data)] }),
            "validation_failed",
            "files",
        ),
        (
            json!({ "parent_snapshot_id": null, "platform": "amiga", "files": [] }),
            "validation_failed",
            "platform",
        ),
    ];
    for (body, code, field) in cases {
        let (status, resp) =
            call(&e, "POST", &format!("/v1/saves/{p}/snapshots"), Some(&body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} → {resp}");
        assert_eq!(resp["code"], code, "{resp}");
        assert_eq!(resp["errors"][0]["field"], field, "{resp}");
    }

    let (status, resp) = call(
        &e,
        "POST",
        &format!("/v1/saves/{p}/blobs/prepare"),
        Some(&json!({ "blobs": [{ "blake3": "ABC", "size": -1 }] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let fields: Vec<&str> = resp["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["field"].as_str().unwrap())
        .collect();
    assert_eq!(fields, vec!["blobs[0].blake3", "blobs[0].size"]);

    let (status, resp) = call(
        &e,
        "POST",
        &format!("/v1/saves/{p}/blobs/download-urls"),
        Some(&json!({ "blake3": [hash(data), hash(data)] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(resp["errors"][0]["code"], "duplicate");

    // Unknown package.
    let unknown = Uuid::now_v7();
    let (status, _) = call(
        &e,
        "POST",
        &format!("/v1/saves/{unknown}/blobs/prepare"),
        Some(&json!({ "blobs": [] })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &e,
        "POST",
        &format!("/v1/saves/{unknown}/snapshots"),
        Some(&json!({ "parent_snapshot_id": null, "platform": "linux", "files": [] })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // No credentials.
    let resp = send(&e.app, get_req(&format!("/v1/saves/{p}/head"))).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn a_full_size_snapshot_fits_the_body_limit(pool: PgPool) {
    let e = env(pool).await;
    let data: &[u8] = b"shared content";
    push_blobs(&e, &[data]).await;
    // 10,000 files (the maximum) is well over the default 1 MiB JSON limit.
    let files: Vec<Value> = (0..saves::MAX_FILES)
        .map(|i| {
            file(
                "saves",
                &format!("slots/profile-{i:05}/data-file.sav"),
                data,
            )
        })
        .collect();
    let body = json!({ "parent_snapshot_id": null, "platform": "windows", "files": files });
    assert!(serde_json::to_vec(&body).unwrap().len() > 1024 * 1024);
    let (status, resp) = call(
        &e,
        "POST",
        &format!("/v1/saves/{}/snapshots", e.package),
        Some(&body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", resp["code"]);
    assert_eq!(resp["file_count"], saves::MAX_FILES);

    let mut too_many = body.clone();
    too_many["files"]
        .as_array_mut()
        .unwrap()
        .push(file("saves", "one-more.sav", data));
    let (status, resp) = call(
        &e,
        "POST",
        &format!("/v1/saves/{}/snapshots", e.package),
        Some(&too_many),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
    assert_eq!(resp["errors"][0]["field"], "files");
}

#[sqlx::test(migrations = "./migrations")]
async fn commit_replays_with_idempotency_key(pool: PgPool) {
    let e = env(pool).await;
    let data: &[u8] = b"idempotent";
    push_blobs(&e, &[data]).await;
    let body =
        json!({ "parent_snapshot_id": null, "platform": "macos", "files": [file("s", "a", data)] });
    let request = || {
        let mut r = req(
            "POST",
            &format!("/v1/saves/{}/snapshots", e.package),
            &e.token,
            Some(&body),
        );
        r.headers_mut()
            .insert("idempotency-key", "save-push-0001-abcdef".parse().unwrap());
        r
    };
    let first = send(&e.app, request()).await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first = body_json(first).await;
    // A retry after a lost response gets the same snapshot, not a head conflict.
    let again = send(&e.app, request()).await;
    assert_eq!(again.status(), StatusCode::CREATED);
    assert_eq!(again.headers()["idempotent-replayed"], "true");
    assert_eq!(body_json(again).await, first);
}
