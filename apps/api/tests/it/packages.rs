//! Packages, assets and the public catalog (A1-T09).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::common;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

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

async fn create(app: &Router, token: &str, body: Value) -> (StatusCode, Value, Option<String>) {
    let resp = send(
        app,
        authed("POST", "/v1/admin/packages", token, Some(&body)),
    )
    .await;
    let status = resp.status();
    let etag = resp
        .headers()
        .get(header::ETAG)
        .map(|v| v.to_str().unwrap().to_string());
    (status, body_json(resp).await, etag)
}

async fn patch(
    app: &Router,
    token: &str,
    id: &str,
    etag: Option<&str>,
    body: Value,
) -> axum::http::Response<Body> {
    let mut req = authed(
        "PATCH",
        &format!("/v1/admin/packages/{id}"),
        token,
        Some(&body),
    );
    if let Some(e) = etag {
        req.headers_mut()
            .insert(header::IF_MATCH, e.parse().unwrap());
    }
    send(app, req).await
}

/// Seeds a published linux release so the package shows up in the public catalog.
async fn seed_release(pool: &PgPool, package_id: Uuid, admin: Uuid) {
    sqlx::query("INSERT INTO trust_bundles (version, bundle, signature) VALUES (1, '\\x00', $1) ON CONFLICT DO NOTHING")
        .bind(vec![0u8; 64])
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO publisher_keys (key_id, public_key, holder_user_id, label, not_before, not_after, trust_version)
         VALUES ('0123456789abcdef0123456789abcdef', $1, $2, 'test', now() - interval '1 day', now() + interval '1 year', 1)
         ON CONFLICT DO NOTHING",
    )
    .bind(vec![7u8; 32])
    .bind(admin)
    .execute(pool)
    .await
    .unwrap();
    let version: Uuid = sqlx::query_scalar(
        "INSERT INTO package_versions (package_id, platform, sequence, version_label, state, pack_count, total_size,
            file_count, chunk_count, manifest_object, manifest_size, manifest_blake3, signature, publisher_key_id,
            created_by, published_at)
         VALUES ($1, 'linux-x86_64', 1, '1.0', 'published', 1, 1234, 1, 1, 'm', 10, $2, $3,
            '0123456789abcdef0123456789abcdef', $4, now())
         RETURNING id",
    )
    .bind(package_id)
    .bind(vec![1u8; 32])
    .bind(vec![2u8; 64])
    .bind(admin)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO package_releases (package_id, platform, version_id, updated_by) VALUES ($1, 'linux-x86_64', $2, $3)")
        .bind(package_id)
        .bind(version)
        .bind(admin)
        .execute(pool)
        .await
        .unwrap();
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(width, height, image::Rgb([200, 30, 60]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn multipart(token: &str, package_id: &str, kind: &str, file: &[u8]) -> Request<Body> {
    let boundary = "vgamesTestBoundary7MA4YWxk";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"kind\"\r\n\r\n{kind}\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"x\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Request::builder()
        .method("POST")
        .uri(format!("/v1/admin/packages/{package_id}/assets"))
        .header("authorization", format!("Bearer {token}"))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn create_validates_slugifies_and_resolves_collisions(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, admin) = seed_session(&pool, "admin").await;

    let (status, body, etag) = create(&app, &admin, json!({ "title": "Half-Life: Alyx" })).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["slug"], "half-life-alyx");
    assert_eq!(body["status"], "draft");
    assert!(etag.unwrap().starts_with("W/\""));

    // Same title again: a suffixed slug instead of a conflict.
    let (status, body, _) = create(&app, &admin, json!({ "title": "Half-Life: Alyx" })).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["slug"], "half-life-alyx-2");

    // An explicit slug that is taken is a conflict.
    let (status, body, _) = create(
        &app,
        &admin,
        json!({ "title": "Other", "slug": "half-life-alyx" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "urn:vgames:problem:slug_taken");

    let (status, body, _) = create(&app, &admin, json!({ "title": "", "slug": "Bad Slug" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let fields: Vec<&str> = body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["field"].as_str().unwrap())
        .collect();
    assert!(
        fields.contains(&"title") && fields.contains(&"slug"),
        "{body}"
    );

    let (status, _, _) = create(&app, &admin, json!({ "title": "x", "surprise": 1 })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "./migrations")]
async fn admin_endpoints_require_admin(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, user) = seed_session(&pool, "user").await;
    let (status, _, _) = create(&app, &user, json!({ "title": "Nope" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let resp = send(&app, authed("GET", "/v1/admin/packages", &user, None)).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = send(&app, get_req("/v1/admin/packages")).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn create_is_idempotent(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, admin) = seed_session(&pool, "admin").await;
    let key = "pkg-create-0001-abcdef";
    let req = |title: &str| {
        let mut r = authed(
            "POST",
            "/v1/admin/packages",
            &admin,
            Some(&json!({ "title": title })),
        );
        r.headers_mut()
            .insert("idempotency-key", key.parse().unwrap());
        r
    };
    let first = send(&app, req("Portal")).await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first = body_json(first).await;
    let replay = send(&app, req("Portal")).await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert!(replay.headers().contains_key(header::ETAG));
    assert_eq!(body_json(replay).await["id"], first["id"]);

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM packages")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    // Same key, different body.
    let other = send(&app, req("Portal 2")).await;
    assert_eq!(other.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(other).await["type"],
        "urn:vgames:problem:idempotency_key_reused"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn update_needs_matching_if_match(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, admin) = seed_session(&pool, "admin").await;
    let (_, pkg, etag) = create(&app, &admin, json!({ "title": "Celeste" })).await;
    let id = pkg["id"].as_str().unwrap();
    let etag = etag.unwrap();

    let resp = patch(&app, &admin, id, None, json!({ "summary": "Climb" })).await;
    assert_eq!(resp.status(), StatusCode::PRECONDITION_REQUIRED);

    let resp = patch(
        &app,
        &admin,
        id,
        Some("W/\"1\""),
        json!({ "summary": "Climb" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::PRECONDITION_FAILED);

    let resp = patch(
        &app,
        &admin,
        id,
        Some(&etag),
        json!({ "summary": "Climb", "genres": ["platformer", " platformer "] }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let new_etag = resp
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_ne!(new_etag, etag);
    let body = body_json(resp).await;
    assert_eq!(body["summary"], "Climb");
    assert_eq!(body["genres"], json!(["platformer"]));
    assert_eq!(body["field_sources"]["summary"], "admin");

    // The old tag is stale now: a concurrent editor loses.
    let resp = patch(&app, &admin, id, Some(&etag), json!({ "summary": "Other" })).await;
    assert_eq!(resp.status(), StatusCode::PRECONDITION_FAILED);

    // Explicit null clears a field.
    let resp = patch(
        &app,
        &admin,
        id,
        Some(&new_etag),
        json!({ "summary": null }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        body_json(resp)
            .await
            .get("summary")
            .is_none_or(Value::is_null)
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn delete_hides_package(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, admin) = seed_session(&pool, "admin").await;
    let (_, pkg, etag) = create(&app, &admin, json!({ "title": "Doomed" })).await;
    let id = pkg["id"].as_str().unwrap();

    let resp = send(
        &app,
        authed("DELETE", &format!("/v1/admin/packages/{id}"), &admin, None),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::PRECONDITION_REQUIRED);

    let mut req = authed("DELETE", &format!("/v1/admin/packages/{id}"), &admin, None);
    req.headers_mut()
        .insert(header::IF_MATCH, etag.unwrap().parse().unwrap());
    assert_eq!(send(&app, req).await.status(), StatusCode::NO_CONTENT);

    let resp = send(
        &app,
        authed("GET", &format!("/v1/admin/packages/{id}"), &admin, None),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    // Deleted packages keep their slug, so old links never point at a different game.
    let (status, body, _) = create(&app, &admin, json!({ "title": "Doomed" })).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["slug"], "doomed-2");
}

#[sqlx::test(migrations = "./migrations")]
async fn catalog_shows_only_published_packages_with_releases(pool: PgPool) {
    let app = app(pool.clone());
    let (admin_id, _, admin) = seed_session(&pool, "admin").await;
    let (_, _, user) = seed_session(&pool, "user").await;

    let mut ids = Vec::new();
    for title in ["Beta Game", "alpha game", "Gamma Draft", "Delta No Release"] {
        let (_, pkg, etag) = create(&app, &admin, json!({ "title": title })).await;
        let id = pkg["id"].as_str().unwrap().to_string();
        if title != "Gamma Draft" {
            let resp = patch(
                &app,
                &admin,
                &id,
                etag.as_deref(),
                json!({ "status": "published", "genres": ["rpg"] }),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
        }
        if title != "Delta No Release" {
            seed_release(&pool, id.parse().unwrap(), admin_id).await;
        }
        ids.push(id);
    }

    let resp = send(&app, get_req("/v1/packages")).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let page = body_json(send(&app, authed("GET", "/v1/packages", &user, None)).await).await;
    let titles: Vec<&str> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["alpha game", "Beta Game"]);
    assert_eq!(page["items"][0]["platforms"], json!(["linux-x86_64"]));

    // Pagination with a signed cursor.
    let first =
        body_json(send(&app, authed("GET", "/v1/packages?limit=1", &user, None)).await).await;
    assert_eq!(first["items"][0]["title"], "alpha game");
    let cursor = first["next_cursor"].as_str().unwrap();
    let second = body_json(
        send(
            &app,
            authed(
                "GET",
                &format!("/v1/packages?limit=1&cursor={cursor}"),
                &user,
                None,
            ),
        )
        .await,
    )
    .await;
    assert_eq!(second["items"][0]["title"], "Beta Game");
    assert!(second["next_cursor"].is_null());
    // A cursor is bound to its filters.
    let resp = send(
        &app,
        authed(
            "GET",
            &format!("/v1/packages?limit=1&q=beta&cursor={cursor}"),
            &user,
            None,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let page = body_json(send(&app, authed("GET", "/v1/packages?q=BETA", &user, None)).await).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let page = body_json(send(&app, authed("GET", "/v1/packages?q=%25", &user, None)).await).await;
    assert_eq!(
        page["items"].as_array().unwrap().len(),
        0,
        "LIKE wildcards are escaped"
    );
    let page = body_json(
        send(
            &app,
            authed("GET", "/v1/packages?platform=windows-x86_64", &user, None),
        )
        .await,
    )
    .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 0);

    // Detail: users see published packages only; admins may preview drafts.
    let resp = send(
        &app,
        authed("GET", &format!("/v1/packages/{}", ids[0]), &user, None),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let detail = body_json(resp).await;
    assert_eq!(detail["releases"][0]["version_label"], "1.0");
    for hidden in [&ids[2], &ids[3]] {
        let resp = send(
            &app,
            authed("GET", &format!("/v1/packages/{hidden}"), &user, None),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
    let resp = send(
        &app,
        authed("GET", &format!("/v1/packages/{}", ids[2]), &admin, None),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let page = body_json(
        send(
            &app,
            authed("GET", "/v1/admin/packages?status=draft", &admin, None),
        )
        .await,
    )
    .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn asset_upload_reencodes_dedupes_and_redirects(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, admin) = seed_session(&pool, "admin").await;
    let (_, pkg, _) = create(&app, &admin, json!({ "title": "Pictures" })).await;
    let id = pkg["id"].as_str().unwrap();

    let image = png(64, 32);
    let resp = send(&app, multipart(&admin, id, "cover", &image)).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let asset = body_json(resp).await;
    assert_eq!(asset["kind"], "cover");
    assert_eq!(asset["width"], 64);
    assert_eq!(asset["height"], 32);
    assert_eq!(asset["content_type"], "image/png");

    // The first cover becomes the package cover.
    let resp = send(
        &app,
        authed("GET", &format!("/v1/admin/packages/{id}"), &admin, None),
    )
    .await;
    assert_eq!(body_json(resp).await["cover"]["id"], asset["id"]);

    // Same bytes again: the existing asset, not a new row.
    let resp = send(&app, multipart(&admin, id, "cover", &image)).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(body_json(resp).await["id"], asset["id"]);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM package_assets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    // Fetch through the redirect.
    let asset_id = asset["id"].as_str().unwrap();
    let resp = send(
        &app,
        authed("GET", &format!("/v1/assets/{asset_id}"), &admin, None),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FOUND);
    let location = resp
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        resp.headers()
            .get(header::CACHE_CONTROL)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("private")
    );
    let path = location
        .strip_prefix("http://localhost:8080")
        .unwrap_or(&location);
    let resp = send(&app, get_req(path)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    let decoded = image::load_from_memory(&bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (64, 32));

    // Delete.
    let resp = send(
        &app,
        authed(
            "DELETE",
            &format!("/v1/admin/assets/{asset_id}"),
            &admin,
            None,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = send(
        &app,
        authed("GET", &format!("/v1/assets/{asset_id}"), &admin, None),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "./migrations")]
async fn asset_upload_rejects_bad_images(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, admin) = seed_session(&pool, "admin").await;
    let (_, pkg, _) = create(&app, &admin, json!({ "title": "Bad Pictures" })).await;
    let id = pkg["id"].as_str().unwrap();

    let gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00;";
    let resp = send(&app, multipart(&admin, id, "cover", gif)).await;
    assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // Tiny file, huge dimensions: rejected before decoding the pixels.
    let resp = send(&app, multipart(&admin, id, "hero", &png(16_385, 1))).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let mut truncated = png(8, 8);
    truncated.truncate(truncated.len() / 2);
    let resp = send(&app, multipart(&admin, id, "logo", &truncated)).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = send(&app, multipart(&admin, id, "banner", &png(8, 8))).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = send(
        &app,
        multipart(&admin, &Uuid::now_v7().to_string(), "cover", &png(8, 8)),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Not multipart at all: a problem+json 415, not axum's plain-text rejection.
    let resp = send(
        &app,
        common::publishing::json_req(
            "POST",
            &format!("/v1/admin/packages/{id}/assets"),
            &admin,
            &json!({}),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(resp.headers()["content-type"], "application/problem+json");
    assert_eq!(body_json(resp).await["code"], "unsupported_media_type");

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM package_assets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn release_dates_are_iso_strings(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, admin) = seed_session(&pool, "admin").await;
    let (_, pkg, etag) = create(&app, &admin, json!({ "title": "Dated" })).await;
    let id = pkg["id"].as_str().unwrap();

    let resp = patch(
        &app,
        &admin,
        id,
        etag.as_deref(),
        json!({ "release_date": "25/01/2018" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = patch(
        &app,
        &admin,
        id,
        etag.as_deref(),
        json!({ "release_date": "2018-01-25" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(body_json(resp).await["release_date"], "2018-01-25");

    let resp = patch(
        &app,
        &admin,
        id,
        Some(&etag),
        json!({ "release_date": null }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        body_json(resp)
            .await
            .get("release_date")
            .is_none_or(Value::is_null)
    );
}
