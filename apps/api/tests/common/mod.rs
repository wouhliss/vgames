//! Shared helpers for integration tests.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use axum::{
    Router,
    body::Body,
    http::{Request, Response},
};
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;
use vgames_api::{AppState, Config};

/// A public key whose private half was discarded (only used where no signature is needed).
pub const DEV_ROOT_KEY: &str = "sn/b0D2mU+3RfBglb8/Jy/2BfTWHvE2SyIgXkx3m8U8=";

pub fn env_map() -> HashMap<&'static str, String> {
    let data = std::env::temp_dir().join(format!("vgames-test-{}", uuid::Uuid::now_v7()));
    HashMap::from([
        ("VGAMES_PUBLIC_URL", "http://localhost:8080".to_string()),
        (
            "VGAMES_SERVER_ID",
            "01920000-0000-7000-8000-00000000abcd".to_string(),
        ),
        (
            "DATABASE_URL",
            "postgres://unused@localhost/unused".to_string(),
        ),
        ("VGAMES_ROOT_PUBLIC_KEY", DEV_ROOT_KEY.to_string()),
        (
            "VGAMES_SERVER_SECRET",
            "dGVzdC1zZXJ2ZXItc2VjcmV0LXRlc3Qtc2VydmVyLXNlY3JldA==".to_string(),
        ),
        ("VGAMES_DEV_FAKE_DISCORD", "true".to_string()),
        ("VGAMES_STORAGE_BACKEND", "fs".to_string()),
        ("VGAMES_FS_STORAGE_ROOT", data.display().to_string()),
        (
            "VGAMES_FS_URL_SIGNING_KEY",
            "dGVzdC1mcy1zaWduaW5nLWtleS10ZXN0LWZzLXNpZ25pbmc=".to_string(),
        ),
    ])
}

pub fn config_with(overrides: &[(&'static str, &str)]) -> Config {
    let mut m = env_map();
    for (k, v) in overrides {
        m.insert(k, (*v).to_string());
    }
    Config::from_lookup(|k| m.get(k).cloned()).expect("test config is valid")
}

pub fn test_config() -> Config {
    config_with(&[])
}

pub fn state(pool: PgPool) -> AppState {
    AppState::new(test_config(), pool).expect("state")
}

pub fn app(pool: PgPool) -> Router {
    vgames_api::http::router(state(pool))
}

/// A pool that never connects (for tests that must not touch the database).
pub fn lazy_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(200))
        .connect_lazy("postgres://nobody@127.0.0.1:1/none")
        .expect("lazy pool")
}

pub async fn send(app: &Router, req: Request<Body>) -> Response<Body> {
    app.clone().oneshot(req).await.expect("infallible")
}

pub async fn body_json(resp: Response<Body>) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&bytes)))
}

pub fn get_req(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("request")
}

pub fn json_request(method: &str, uri: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).expect("json")))
        .expect("request")
}
