//! Release descriptor, pack download URLs and integrity reports (A1-T12 part 2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::{Arc, Mutex};

use crate::common::{publishing::*, *};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use vgames_api::jobs::{self, Registry};
use vgames_core::{Context, Digest, Envelope};

struct Published {
    version: Uuid,
    built: Built,
}

/// Uploads, finalizes, verifies and publishes a version; the package itself is published too.
async fn published(w: &World, pool: &PgPool) -> Published {
    let (version, built) = full_upload(w, false).await;
    let (status, body) = finalize(
        w,
        version,
        &built.manifest,
        envelope(&key(2), &built.manifest),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let registry = Registry::standard();
    while jobs::run_one(&w.state, &registry, "test").await.unwrap() {}
    let resp = send(
        &w.app,
        bearer_request(
            "POST",
            &format!("/v1/admin/versions/{version}/publish"),
            &w.owner,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    sqlx::query("UPDATE packages SET status = 'published' WHERE id = $1")
        .bind(w.package)
        .execute(pool)
        .await
        .unwrap();
    Published { version, built }
}

/// GETs a signed storage URL through the router.
async fn fetch(w: &World, url: &str) -> (StatusCode, Vec<u8>) {
    let req = Request::builder()
        .uri(url.strip_prefix(ORIGIN).unwrap_or(url))
        .body(Body::empty())
        .unwrap();
    let resp = send(&w.app, req).await;
    let status = resp.status();
    (
        status,
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
}

async fn urls(w: &World, token: &str, version: Uuid, packs: Value) -> (StatusCode, Value) {
    let resp = send(
        &w.app,
        json_req(
            "POST",
            &format!("/v1/versions/{version}/download-urls"),
            token,
            &json!({ "packs": packs }),
        ),
    )
    .await;
    let status = resp.status();
    (status, body_json(resp).await)
}

#[sqlx::test(migrations = "./migrations")]
async fn players_get_a_verifiable_release_and_pack_urls(pool: PgPool) {
    let w = world(&pool).await;
    let (_, _, player) = seed_session(&pool, "user").await;
    let uri = format!("/v1/packages/{}/releases/linux-x86_64", w.package);
    assert_eq!(
        send(&w.app, bearer_request("GET", &uri, &player))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    let p = published(&w, &pool).await;
    assert_eq!(
        send(&w.app, get_req(&uri)).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let d = body_json(send(&w.app, bearer_request("GET", &uri, &player)).await).await;
    assert_eq!(d["version_id"], p.version.to_string());
    assert_eq!(d["pack_count"], p.built.packs.len());
    assert!(
        d.get("yanked_version_ids")
            .is_none_or(|v| v.as_array().unwrap().is_empty())
    );

    // The manifest behind the signed URL is the signed one.
    let (status, manifest) = fetch(&w, d["manifest"]["url"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(manifest, p.built.manifest);
    assert_eq!(d["manifest"]["blake3"], Digest::of(&manifest).to_hex());
    let env = Envelope::parse(&serde_json::to_vec(&d["signature"]).unwrap()).unwrap();
    env.verify(&key(2).public_key(), Context::Manifest, &manifest)
        .unwrap();

    // Pack URLs return the exact pack bytes.
    let (status, list) = urls(&w, &player, p.version, json!([0])).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let item = &list["items"][0];
    assert_eq!(item["size"], p.built.packs[0].len());
    let (status, bytes) = fetch(&w, item["url"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, p.built.packs[0]);
    for bad in [
        json!([]),
        json!([99]),
        json!([100000]),
        json!((0..501).collect::<Vec<u32>>()),
    ] {
        assert_eq!(
            urls(&w, &player, p.version, bad.clone()).await.0,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }

    // A newer version replaces it; yanking it makes launchers fall back and refuses its URLs.
    let p2 = published(&w, &pool).await;
    let resp = send(
        &w.app,
        json_req(
            "POST",
            &format!("/v1/admin/versions/{}/yank", p2.version),
            &w.owner,
            &json!({ "reason": "broken build" }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let d = body_json(send(&w.app, bearer_request("GET", &uri, &player)).await).await;
    assert_eq!(d["version_id"], p.version.to_string());
    assert_eq!(d["yanked_version_ids"], json!([p2.version.to_string()]));
    let (status, body) = urls(&w, &player, p2.version, json!([0])).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::GONE, Some("version_yanked"))
    );

    // Versions that are not published are invisible to players.
    let (v3, _) = full_upload(&w, false).await;
    assert_eq!(
        urls(&w, &player, v3, json!([0])).await.0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn three_distinct_reporters_trigger_a_reverify(pool: PgPool) {
    let w = world(&pool).await;
    let p = published(&w, &pool).await;
    let report = |token: String| {
        let (app, version) = (w.app.clone(), p.version);
        async move {
            let body =
                json!({ "pack_index": 0, "chunk_index": 1, "detail": "hash mismatch twice" });
            send(
                &app,
                json_req(
                    "POST",
                    &format!("/v1/versions/{version}/integrity-reports"),
                    &token,
                    &body,
                ),
            )
            .await
            .status()
        }
    };
    let reverify_jobs = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE kind = 'pack.reverify'")
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let (_, _, a) = seed_session(&pool, "user").await;
    let (_, _, b) = seed_session(&pool, "user").await;
    let (_, _, c) = seed_session(&pool, "user").await;
    for token in [&a, &a, &a, &b] {
        assert_eq!(report(token.clone()).await, StatusCode::ACCEPTED);
    }
    assert_eq!(
        reverify_jobs().await,
        0,
        "the same user reporting again does not count"
    );
    assert_eq!(report(c.clone()).await, StatusCode::ACCEPTED);
    assert_eq!(reverify_jobs().await, 1);
    assert_eq!(report(c.clone()).await, StatusCode::ACCEPTED);
    assert_eq!(reverify_jobs().await, 1, "deduplicated while queued");

    let body = json!({ "pack_index": 9 });
    let resp = send(
        &w.app,
        json_req(
            "POST",
            &format!("/v1/versions/{}/integrity-reports", p.version),
            &a,
            &body,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let registry = Registry::standard();
    while jobs::run_one(&w.state, &registry, "test").await.unwrap() {}
    let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE kind = 'pack.reverify'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "succeeded");
}

/// Collects everything logged while it is the default subscriber of this thread.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn signed_urls_are_never_logged(pool: PgPool) {
    let w = world(&pool).await;
    let p = published(&w, &pool).await;
    let capture = Capture::default();
    let sink = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || sink.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let uri = format!("/v1/packages/{}/releases/linux-x86_64", w.package);
    let d = body_json(send(&w.app, bearer_request("GET", &uri, &w.owner)).await).await;
    let (_, list) = urls(&w, &w.owner, p.version, json!([0])).await;
    let logged = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(!logged.is_empty(), "the capture works");
    for url in [
        d["manifest"]["url"].as_str().unwrap(),
        list["items"][0]["url"].as_str().unwrap(),
    ] {
        let query = url.split_once('?').map_or(url, |(_, q)| q);
        assert!(!logged.contains(query), "a signed URL leaked into the logs");
    }
}
