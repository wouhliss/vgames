//! Shared helpers for integration tests.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod publishing;

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

/// Creates a user and a desktop session; returns `(user_id, session_id, access_token)`.
pub async fn seed_session(pool: &PgPool, role: &str) -> (uuid::Uuid, uuid::Uuid, String) {
    use sha2::{Digest, Sha256};
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).unwrap();
    let token = format!(
        "vga_{}",
        base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, raw)
    );
    let discord_id = format!(
        "{}",
        600_000_000_000_000_000u64 + u64::from(raw[0]) * 1000 + u64::from(raw[1])
    );
    let user_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO users (discord_id, username, role) VALUES ($1, 'seeded', $2) RETURNING id",
    )
    .bind(&discord_id)
    .bind(role)
    .fetch_one(pool)
    .await
    .unwrap();
    let mut refresh = [0u8; 32];
    getrandom::fill(&mut refresh).unwrap();
    let session_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO sessions (user_id, kind, access_token_hash, access_expires_at, refresh_token_hash, refresh_expires_at)
         VALUES ($1, 'desktop', $2, now() + interval '15 minutes', $3, now() + interval '30 days') RETURNING id",
    )
    .bind(user_id)
    .bind(Sha256::digest(token.as_bytes()).to_vec())
    .bind(Sha256::digest(refresh).to_vec())
    .fetch_one(pool)
    .await
    .unwrap();
    (user_id, session_id, token)
}

pub fn bearer_request(method: &str, uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}
