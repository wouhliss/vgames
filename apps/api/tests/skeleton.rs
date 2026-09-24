//! A1-T01: service skeleton behaviour.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::Duration;

use axum::{Router, http::StatusCode, routing::get};
use common::*;
use sqlx::PgPool;

#[sqlx::test(migrations = "./migrations")]
async fn health_is_ok_with_a_database(pool: PgPool) {
    let resp = send(&app(pool), get_req("/v1/health")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let h = resp.headers();
    assert!(h.contains_key("x-request-id"));
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["referrer-policy"], "no-referrer");
    assert!(
        !h.contains_key("strict-transport-security"),
        "no HSTS over http://localhost"
    );
    let body = body_json(resp).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["db"], "ok");
}

#[sqlx::test(migrations = "./migrations")]
async fn health_is_503_when_the_database_is_down(pool: PgPool) {
    let app = app(pool.clone());
    pool.close().await;
    let resp = send(&app, get_req("/v1/health")).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = body_json(resp).await;
    assert_eq!(body["status"], "down");
    assert_eq!(body["db"], "down");
}

#[tokio::test]
async fn unknown_routes_are_problem_json() {
    let resp = send(&app(lazy_pool()), get_req("/v1/nope?secret=1")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(resp.headers()["content-type"], "application/problem+json");
    let rid = resp.headers()["x-request-id"].to_str().unwrap().to_string();
    let body = body_json(resp).await;
    assert_eq!(body["type"], "urn:vgames:problem:not_found");
    assert_eq!(body["code"], "not_found");
    assert_eq!(body["status"], 404);
    assert_eq!(body["instance"], "/v1/nope");
    assert_eq!(body["request_id"], rid);
}

#[tokio::test]
async fn wrong_method_is_405_problem() {
    let req = axum::http::Request::builder()
        .method("DELETE")
        .uri("/v1/health")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = send(&app(lazy_pool()), req).await;
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body_json(resp).await["code"], "method_not_allowed");
}

#[tokio::test]
async fn valid_request_ids_are_echoed_and_invalid_ones_replaced() {
    let app = app(lazy_pool());
    let req = axum::http::Request::builder()
        .uri("/v1/nope")
        .header("x-request-id", "client-supplied-123")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = send(&app, req).await;
    assert_eq!(resp.headers()["x-request-id"], "client-supplied-123");
    let req = axum::http::Request::builder()
        .uri("/v1/nope")
        .header("x-request-id", "bad id!")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = send(&app, req).await;
    assert_ne!(resp.headers()["x-request-id"], "bad id!");
}

#[tokio::test]
async fn panics_become_500_problems() {
    let st = state(lazy_pool());
    async fn boom() -> &'static str {
        let fail = std::hint::black_box(true);
        if fail {
            panic!("kaboom");
        }
        "unreachable"
    }
    let routes = Router::new().route("/boom", get(boom));
    let app = vgames_api::http::with_layers(routes, st);
    let resp = send(&app, get_req("/boom")).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "internal");
    assert!(
        body["request_id"].is_string(),
        "request context survives the panic: {body}"
    );
    assert!(
        !body.to_string().contains("kaboom"),
        "panic message must not leak"
    );
}

#[tokio::test(start_paused = true)]
async fn slow_handlers_time_out_with_503() {
    let st = state(lazy_pool());
    let routes = Router::new().route(
        "/slow",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(120)).await;
            "late"
        }),
    );
    let app = vgames_api::http::with_layers(routes, st);
    let resp = send(&app, get_req("/slow")).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body_json(resp).await["code"], "timeout");
}

#[tokio::test]
async fn hsts_is_sent_for_https_servers() {
    let config = config_with(&[
        ("VGAMES_PUBLIC_URL", "https://vgames.example"),
        ("VGAMES_DEV_FAKE_DISCORD", "false"),
        ("DISCORD_CLIENT_ID", "123456789012"),
        ("DISCORD_CLIENT_SECRET", "x"),
        (
            "DISCORD_REDIRECT_URI",
            "https://vgames.example/v1/auth/discord/callback",
        ),
    ]);
    let app = vgames_api::http::router(vgames_api::AppState::new(config, lazy_pool()).unwrap());
    let resp = send(&app, get_req("/v1/nope")).await;
    assert_eq!(
        resp.headers()["strict-transport-security"],
        "max-age=63072000; includeSubDomains"
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "needs vgames-core fingerprint (A5-T03); until then the endpoint answers 503"]
async fn well_known_describes_the_server(pool: PgPool) {
    let resp = send(&app(pool), get_req("/.well-known/vgames.json")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["format"], "vgames.server/1");
    assert_eq!(body["registration_mode"], "allowlist");
    assert!(
        body["root_key_fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("VG1-")
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn openapi_and_swagger_are_served(pool: PgPool) {
    let app = app(pool);
    let resp = send(&app, get_req("/openapi.json")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let doc = body_json(resp).await;
    assert!(doc["paths"]["/v1/health"]["get"].is_object());
    let resp = send(&app, get_req("/docs/")).await;
    assert_eq!(resp.status(), StatusCode::OK);
}
