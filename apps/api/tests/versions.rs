//! Version creation, upload targets and abort (A1-T11 part 1).
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
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use vgames_api::{
    AppState,
    jobs::{self, Registry},
    storage::BucketKind,
    versions::{manifest_object, pack_object},
};

const ORIGIN: &str = "http://localhost:8080";

fn authed(method: &str, uri: &str, token: &str, body: Option<&Value>) -> Request<Body> {
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

async fn setup(pool: &PgPool) -> (AppState, Router, (Uuid, String), String) {
    let state = state(pool.clone());
    let app = vgames_api::http::router(state.clone());
    let (admin_id, _, admin) = seed_session(pool, "admin").await;
    let resp = send(
        &app,
        authed(
            "POST",
            "/v1/admin/packages",
            &admin,
            Some(&json!({ "title": "Game", "fetch_metadata": false })),
        ),
    )
    .await;
    let package = body_json(resp).await["id"].as_str().unwrap().to_string();
    (state, app, (admin_id, admin), package)
}

async fn create(
    app: &Router,
    token: &str,
    package: &str,
    platform: &str,
    label: &str,
) -> (StatusCode, Value) {
    let body = json!({ "platform": platform, "version_label": label });
    let resp = send(
        app,
        authed(
            "POST",
            &format!("/v1/admin/packages/{package}/versions"),
            token,
            Some(&body),
        ),
    )
    .await;
    let status = resp.status();
    (status, body_json(resp).await)
}

/// Sends a signed storage request to the fs backend mounted on the same router.
async fn storage_call(
    app: &Router,
    method: &str,
    url: &str,
    headers: &[(&str, String)],
    body: Vec<u8>,
) -> axum::http::Response<Body> {
    let path = url.strip_prefix(ORIGIN).unwrap_or(url);
    let mut b = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        b = b.header(*k, v);
    }
    send(app, b.body(Body::from(body)).unwrap()).await
}

fn target_headers(t: &Value) -> Vec<(&str, String)> {
    t["headers"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str().unwrap().to_string()))
        .collect()
}

#[sqlx::test(migrations = "./migrations")]
async fn versions_get_sequences_per_platform(pool: PgPool) {
    let (_, app, (admin_id, admin), package) = setup(&pool).await;

    let (status, v1) = create(&app, &admin, &package, "linux-x86_64", "1.0").await;
    assert_eq!(status, StatusCode::CREATED, "{v1}");
    assert_eq!(v1["sequence"], 1);
    assert_eq!(v1["state"], "uploading");
    assert_eq!(v1["server_id"], "01920000-0000-7000-8000-00000000abcd");
    assert_eq!(v1["created_by"]["id"], admin_id.to_string());
    assert_eq!(v1["is_current_release"], false);
    assert_eq!(
        create(&app, &admin, &package, "linux-x86_64", "1.1")
            .await
            .1["sequence"],
        2
    );
    assert_eq!(
        create(&app, &admin, &package, "windows-x86_64", "1.1")
            .await
            .1["sequence"],
        1
    );

    for label in ["", " 1.0", &"x".repeat(65), "a\u{7}b"] {
        assert_eq!(
            create(&app, &admin, &package, "linux-x86_64", label)
                .await
                .0,
            StatusCode::BAD_REQUEST,
            "{label:?}"
        );
    }
    assert_eq!(
        create(&app, &admin, &package, "amiga-68k", "1").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        create(
            &app,
            &admin,
            &Uuid::now_v7().to_string(),
            "linux-x86_64",
            "1"
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (_, _, user) = seed_session(&pool, "user").await;
    assert_eq!(
        create(&app, &user, &package, "linux-x86_64", "1").await.0,
        StatusCode::FORBIDDEN
    );

    // Idempotent retries return the same version.
    let req = || {
        let mut r = authed(
            "POST",
            &format!("/v1/admin/packages/{package}/versions"),
            &admin,
            Some(&json!({ "platform": "macos-aarch64", "version_label": "2.0" })),
        );
        r.headers_mut()
            .insert("idempotency-key", "version-create-0001".parse().unwrap());
        r
    };
    let first = body_json(send(&app, req()).await).await;
    let again = body_json(send(&app, req()).await).await;
    assert_eq!(first["id"], again["id"]);

    // Listing: newest first, paginated.
    let page = body_json(
        send(
            &app,
            bearer_request(
                "GET",
                &format!("/v1/admin/packages/{package}/versions?limit=2"),
                &admin,
            ),
        )
        .await,
    )
    .await;
    let labels: Vec<&str> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version_label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["2.0", "1.1"]);
    let cursor = page["next_cursor"].as_str().unwrap();
    let rest = body_json(
        send(
            &app,
            bearer_request(
                "GET",
                &format!("/v1/admin/packages/{package}/versions?limit=2&cursor={cursor}"),
                &admin,
            ),
        )
        .await,
    )
    .await;
    assert_eq!(rest["items"].as_array().unwrap().len(), 2);
    assert!(rest.get("next_cursor").is_none_or(Value::is_null));

    let id = v1["id"].as_str().unwrap();
    let got = body_json(
        send(
            &app,
            bearer_request("GET", &format!("/v1/admin/versions/{id}"), &admin),
        )
        .await,
    )
    .await;
    assert_eq!(got, v1);
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_creation_yields_unique_sequences(pool: PgPool) {
    let (_, app, (_, admin), package) = setup(&pool).await;
    let calls = (0..10).map(|i| {
        let (app, admin, package) = (app.clone(), admin.clone(), package.clone());
        tokio::spawn(async move {
            create(
                &app,
                &admin,
                &package,
                "linux-x86_64",
                &format!("build-{i}"),
            )
            .await
        })
    });
    let mut sequences: Vec<i64> = Vec::new();
    for c in calls {
        let (status, v) = c.await.unwrap();
        assert_eq!(status, StatusCode::CREATED, "{v}");
        sequences.push(v["sequence"].as_i64().unwrap());
    }
    sequences.sort_unstable();
    assert_eq!(sequences, (1..=10).collect::<Vec<i64>>());
}

#[sqlx::test(migrations = "./migrations")]
async fn uploads_land_under_the_version_and_abort_cleans_up(pool: PgPool) {
    let (state, app, (_, admin), package) = setup(&pool).await;
    let (_, v) = create(&app, &admin, &package, "linux-x86_64", "1.0").await;
    let version = v["id"].as_str().unwrap().to_string();
    let (pid, vid): (Uuid, Uuid) = (package.parse().unwrap(), version.parse().unwrap());

    // Pack 3 through a resumable session.
    let t = body_json(
        send(
            &app,
            bearer_request(
                "POST",
                &format!("/v1/admin/versions/{version}/packs/3/upload-session"),
                &admin,
            ),
        )
        .await,
    )
    .await;
    assert_eq!(t["method"], "POST");
    assert_eq!(t["headers"]["x-goog-resumable"], "start");
    let start = storage_call(
        &app,
        "POST",
        t["url"].as_str().unwrap(),
        &target_headers(&t),
        Vec::new(),
    )
    .await;
    assert_eq!(start.status(), StatusCode::CREATED);
    let session = start.headers()["location"].to_str().unwrap().to_string();
    let data = vec![7u8; 5000];
    let put = storage_call(
        &app,
        "PUT",
        &session,
        &[("content-range", "bytes 0-4999/5000".to_string())],
        data,
    )
    .await;
    assert_eq!(put.status(), StatusCode::OK);
    let meta = state
        .storage
        .head(BucketKind::Packages, &pack_object(pid, vid, 3))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(meta.size, 5000);

    // The manifest through a single PUT.
    let t = body_json(
        send(
            &app,
            bearer_request(
                "POST",
                &format!("/v1/admin/versions/{version}/manifest-upload"),
                &admin,
            ),
        )
        .await,
    )
    .await;
    assert_eq!(t["method"], "PUT");
    let put = storage_call(
        &app,
        "PUT",
        t["url"].as_str().unwrap(),
        &target_headers(&t),
        b"{\"format\":1}".to_vec(),
    )
    .await;
    assert_eq!(put.status(), StatusCode::OK);
    assert!(
        state
            .storage
            .head(BucketKind::Packages, &manifest_object(pid, vid))
            .await
            .unwrap()
            .is_some()
    );

    // Only the creator uploads; indexes are bounded.
    let (_, _, other) = seed_session(&pool, "admin").await;
    let resp = send(
        &app,
        bearer_request(
            "POST",
            &format!("/v1/admin/versions/{version}/packs/0/upload-session"),
            &other,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = send(
        &app,
        bearer_request(
            "POST",
            &format!("/v1/admin/versions/{version}/packs/100000/upload-session"),
            &admin,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Abort (any admin), then uploads stop and a job deletes the objects.
    assert_eq!(
        send(
            &app,
            bearer_request("DELETE", &format!("/v1/admin/versions/{version}"), &other)
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    let resp = send(
        &app,
        bearer_request(
            "POST",
            &format!("/v1/admin/versions/{version}/manifest-upload"),
            &admin,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(resp).await["type"],
        "urn:vgames:problem:version_not_uploading"
    );
    let registry = Registry::standard();
    assert!(jobs::run_one(&state, &registry, "test").await.unwrap());
    let left = state
        .storage
        .list_prefix(BucketKind::Packages, &format!("v1/{pid}/{vid}/"))
        .await
        .unwrap();
    assert!(left.is_empty(), "{left:?}");
    assert_eq!(
        send(
            &app,
            bearer_request("DELETE", &format!("/v1/admin/versions/{version}"), &admin)
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    let got = body_json(
        send(
            &app,
            bearer_request("GET", &format!("/v1/admin/versions/{version}"), &admin),
        )
        .await,
    )
    .await;
    assert_eq!(got["state"], "aborted");
}

#[sqlx::test(migrations = "./migrations")]
async fn published_versions_cannot_be_aborted(pool: PgPool) {
    let (_, app, (admin_id, admin), package) = setup(&pool).await;
    let (_, v) = create(&app, &admin, &package, "linux-x86_64", "1.0").await;
    let version: Uuid = v["id"].as_str().unwrap().parse().unwrap();
    sqlx::query("INSERT INTO trust_bundles (version, bundle, signature) VALUES (1, '\\x00', $1)")
        .bind(vec![0u8; 64])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO publisher_keys (key_id, public_key, holder_user_id, label, not_before, not_after, trust_version)
         VALUES ('0123456789abcdef0123456789abcdef', $1, $2, 'k', now() - interval '1 day', now() + interval '1 year', 1)",
    )
    .bind(vec![7u8; 32])
    .bind(admin_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE package_versions SET state = 'published', pack_count = 1, total_size = 1, file_count = 1, chunk_count = 1,
            manifest_object = 'm', manifest_size = 1, manifest_blake3 = $2, signature = $3,
            publisher_key_id = '0123456789abcdef0123456789abcdef', published_at = now()
         WHERE id = $1",
    )
    .bind(version)
    .bind(vec![1u8; 32])
    .bind(vec![2u8; 64])
    .execute(&pool)
    .await
    .unwrap();
    let resp = send(
        &app,
        bearer_request("DELETE", &format!("/v1/admin/versions/{version}"), &admin),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(resp).await["type"],
        "urn:vgames:problem:version_published"
    );
}
