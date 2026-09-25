//! A1-T15: the admin SPA at `/admin/` (fallback routing, caching, CSP, traversal).
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
use common::*;
use http_body_util::BodyExt;
use sqlx::PgPool;
use vgames_api::{AppState, Config};

const INDEX: &str = "<!doctype html><title>vgames admin</title><script type=module src=/admin/assets/app-3f2a9c.js></script>";

/// `<tmp>/dist` with a built-looking SPA, plus `<tmp>/secret.txt` outside it and a
/// symlink inside it pointing out.
fn dist() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("dist");
    std::fs::create_dir_all(d.join("assets")).unwrap();
    std::fs::write(d.join("index.html"), INDEX).unwrap();
    std::fs::write(d.join("assets/app-3f2a9c.js"), "console.log(1)").unwrap();
    std::fs::write(d.join("assets/pack_bg-81c2.wasm"), b"\0asm\x01\0\0\0").unwrap();
    std::fs::write(d.join("favicon.ico"), [0u8, 0, 1, 0]).unwrap();
    std::fs::write(d.join(".env"), "SECRET=1").unwrap();
    std::fs::write(tmp.path().join("secret.txt"), "top secret").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(tmp.path(), d.join("escape")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("secret.txt"), d.join("assets/leak.js"))
            .unwrap();
    }
    tmp
}

fn app_with(pool: PgPool, dist: &std::path::Path) -> Router {
    let config = config_with(&[("VGAMES_ADMIN_DIST", dist.to_str().unwrap())]);
    vgames_api::http::router(AppState::new(config, pool).unwrap())
}

async fn get(app: &Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = send(app, get_req(uri)).await;
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[sqlx::test(migrations = "./migrations")]
async fn serves_the_spa_with_fallback_caching_and_csp(pool: PgPool) {
    let tmp = dist();
    let app = app_with(pool, &tmp.path().join("dist"));

    let (status, headers, _) = get(&app, "/admin").await;
    assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(headers["location"], "/admin/");

    for uri in [
        "/admin/",
        "/admin/index.html",
        "/admin/users",
        "/admin/packages/0192-abc/versions",
    ] {
        let (status, headers, body) = get(&app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(body, INDEX, "{uri}");
        assert_eq!(headers["content-type"], "text/html; charset=utf-8");
        assert_eq!(headers["cache-control"], "no-store", "{uri}");
        assert_eq!(
            headers["content-security-policy"],
            vgames_api::admin_web::CSP
        );
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers["x-frame-options"], "DENY");
    }
    assert!(vgames_api::admin_web::CSP.contains("script-src 'self' 'wasm-unsafe-eval'"));
    assert!(vgames_api::admin_web::CSP.contains("frame-ancestors 'none'"));

    let (status, headers, body) = get(&app, "/admin/assets/app-3f2a9c.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "console.log(1)");
    assert_eq!(headers["content-type"], "text/javascript; charset=utf-8");
    assert_eq!(
        headers["cache-control"],
        "public, max-age=31536000, immutable"
    );
    let (_, headers, _) = get(&app, "/admin/assets/pack_bg-81c2.wasm").await;
    assert_eq!(headers["content-type"], "application/wasm");
    let (status, headers, _) = get(&app, "/admin/favicon.ico").await;
    assert_eq!(
        (status, &headers["cache-control"]),
        (StatusCode::OK, &"no-cache".parse().unwrap())
    );

    // A missing file is a problem+json 404, not the SPA.
    let (status, headers, body) = get(&app, "/admin/assets/app-000000.js").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(headers["content-type"], "application/problem+json");
    assert!(body.contains("not_found"));

    // HEAD works; writes are refused.
    let resp = send(
        &app,
        Request::builder()
            .method("HEAD")
            .uri("/admin/")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = send(
        &app,
        json_request("POST", "/admin/", &serde_json::json!({})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[sqlx::test(migrations = "./migrations")]
async fn paths_never_leave_the_dist_directory(pool: PgPool) {
    let tmp = dist();
    let app = app_with(pool, &tmp.path().join("dist"));
    for uri in [
        "/admin/../secret.txt",
        "/admin/..%2Fsecret.txt",
        "/admin/%2e%2e/secret.txt",
        "/admin/%2E%2E%2Fsecret.txt",
        "/admin/assets/../../secret.txt",
        "/admin/..%5Csecret.txt",
        "/admin/.env",
        "/admin/%2eenv",
        "/admin/escape/secret.txt",
        "/admin/assets/leak.js",
        "/admin//etc/passwd",
        "/admin/C:%5Cwindows%5Cwin.ini",
        "/admin/a%00b",
    ] {
        let (status, _, body) = get(&app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
        assert!(
            !body.contains("top secret") && !body.contains("SECRET=1"),
            "{uri}"
        );
        assert!(
            !body.contains("vgames admin"),
            "{uri} must not fall back to the SPA"
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn admin_web_is_off_without_a_dist(pool: PgPool) {
    let app = app(pool);
    let (status, headers, _) = get(&app, "/admin/").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(headers["content-type"], "application/problem+json");
    let info = body_json(send(&app, get_req("/.well-known/vgames.json")).await).await;
    assert!(
        !info["features"]
            .as_array()
            .unwrap()
            .contains(&"admin_web".into())
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn discovery_advertises_the_admin_web(pool: PgPool) {
    let tmp = dist();
    let app = app_with(pool, &tmp.path().join("dist"));
    let info = body_json(send(&app, get_req("/.well-known/vgames.json")).await).await;
    assert!(
        info["features"]
            .as_array()
            .unwrap()
            .contains(&"admin_web".into())
    );
}

#[test]
fn a_missing_or_unbuilt_dist_fails_at_startup() {
    let tmp = tempfile::tempdir().unwrap();
    for (dir, why) in [
        (tmp.path().join("nope"), "cannot open"),
        (tmp.path().to_path_buf(), "index.html"),
    ] {
        let mut m = env_map();
        m.insert("VGAMES_ADMIN_DIST", dir.display().to_string());
        let err = Config::from_lookup(|k| m.get(k).cloned()).unwrap_err();
        assert!(
            err.0
                .iter()
                .any(|e| e.starts_with("VGAMES_ADMIN_DIST") && e.contains(why)),
            "{err:?}"
        );
    }
}
