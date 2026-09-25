//! A1-T02: API conventions (idempotency, rate limits).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::common;

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;
use uuid::Uuid;
use vgames_api::{
    error::ApiError,
    http::idempotency::{self, IdempotencyKey},
};

async fn user(pool: &PgPool) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (discord_id, username) VALUES ($1, 'tester') RETURNING id",
    )
    .bind(format!("{}", 10_000_000_000u64 + u64::from(rand_u16())))
    .fetch_one(pool)
    .await
    .unwrap()
}

fn rand_u16() -> u16 {
    let mut b = [0u8; 2];
    getrandom::fill(&mut b).unwrap();
    u16::from_le_bytes(b)
}

#[sqlx::test(migrations = "./migrations")]
async fn idempotent_replay_runs_the_handler_once(pool: PgPool) {
    let uid = user(&pool).await;
    let key = IdempotencyKey(Some("create-package-0001".into()));
    let calls = Arc::new(AtomicU32::new(0));
    let fp = idempotency::fingerprint("POST", "/v1/x", &serde_json::json!({"title": "Portal"}));

    for expected_replay in [false, true, true] {
        let c = calls.clone();
        let resp = idempotency::run(&pool, uid, &key, fp, || async move {
            c.fetch_add(1, Ordering::SeqCst);
            Ok((StatusCode::CREATED, serde_json::json!({"id": "abc"})))
        })
        .await
        .unwrap();
        assert_eq!(resp.status, StatusCode::CREATED);
        assert_eq!(resp.body["id"], "abc");
        assert_eq!(resp.replayed, expected_replay);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Same key, different request.
    let other = idempotency::fingerprint("POST", "/v1/x", &serde_json::json!({"title": "Other"}));
    let err = idempotency::run(&pool, uid, &key, other, || async {
        Ok((StatusCode::CREATED, serde_json::json!({})))
    })
    .await
    .err()
    .unwrap();
    assert_eq!(err.code, "idempotency_key_reused");
    assert_eq!(err.status, StatusCode::CONFLICT);
}

#[sqlx::test(migrations = "./migrations")]
async fn failures_release_the_key_and_in_progress_is_409(pool: PgPool) {
    let uid = user(&pool).await;
    let key = IdempotencyKey(Some("retry-after-failure-1".into()));
    let fp = [1u8; 32];
    let err = idempotency::run(&pool, uid, &key, fp, || async {
        Err(ApiError::unavailable())
    })
    .await
    .err()
    .unwrap();
    assert_eq!(err.code, "unavailable");
    // The key is free again.
    let ok = idempotency::run(&pool, uid, &key, fp, || async {
        Ok((StatusCode::OK, serde_json::json!(1)))
    })
    .await
    .unwrap();
    assert!(!ok.replayed);

    // A key claimed but not completed (another request in flight).
    sqlx::query("INSERT INTO idempotency_keys (user_id, key, request_fingerprint) VALUES ($1, 'still-running-000001', $2)")
        .bind(uid)
        .bind(&fp[..])
        .execute(&pool)
        .await
        .unwrap();
    let busy = IdempotencyKey(Some("still-running-000001".into()));
    let err = idempotency::run(&pool, uid, &busy, fp, || async {
        Ok((StatusCode::OK, serde_json::json!(1)))
    })
    .await
    .err()
    .unwrap();
    assert_eq!(err.code, "idempotency_in_progress");
}

#[sqlx::test(migrations = "./migrations")]
async fn expired_keys_are_reusable(pool: PgPool) {
    let uid = user(&pool).await;
    let fp = [2u8; 32];
    sqlx::query("INSERT INTO idempotency_keys (user_id, key, request_fingerprint, status_code, response_body, expires_at) VALUES ($1, 'expired-key-0000001', $2, 201, '{}', now() - interval '1 minute')")
        .bind(uid)
        .bind(&[9u8; 32][..])
        .execute(&pool)
        .await
        .unwrap();
    let key = IdempotencyKey(Some("expired-key-0000001".into()));
    let resp = idempotency::run(&pool, uid, &key, fp, || async {
        Ok((StatusCode::CREATED, serde_json::json!("fresh")))
    })
    .await
    .unwrap();
    assert!(!resp.replayed);
    assert_eq!(resp.body, "fresh");
}

#[tokio::test]
async fn auth_routes_are_rate_limited_per_ip() {
    let app = app(lazy_pool());
    for _ in 0..20 {
        let resp = send(&app, get_req("/v1/auth/discord/start")).await;
        assert_ne!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    let resp = send(&app, get_req("/v1/auth/discord/start")).await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(resp.headers().contains_key("retry-after"));
    let body = body_json(resp).await;
    assert_eq!(body["code"], "rate_limited");
    assert!(body["request_id"].is_string());
}

/// A valid session on an authenticated route is limited per user, not per IP: users behind
/// one NAT do not share the 300/min public budget. Credentials on anything else (unknown
/// routes, public routes, a bad token) do not switch the IP limit off (A1-T16).
#[sqlx::test(migrations = "./migrations")]
async fn only_valid_sessions_on_authenticated_routes_skip_the_ip_limit(pool: sqlx::PgPool) {
    let app = app(pool.clone());
    let (_, _, token) = seed_session(&pool, "user").await;
    for _ in 0..305 {
        let resp = send(&app, bearer_request("GET", "/v1/me", &token)).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }
    let junk = |uri: &str| {
        axum::http::Request::builder()
            .uri(uri)
            .header("authorization", "Bearer vga_x")
            .body(axum::body::Body::empty())
            .unwrap()
    };
    let mut limited = false;
    for _ in 0..305 {
        if send(&app, junk("/v1/nope")).await.status() == StatusCode::TOO_MANY_REQUESTS {
            limited = true;
            break;
        }
    }
    assert!(
        limited,
        "an unknown route with a junk token must hit the IP limit"
    );
}

#[test]
fn idempotency_key_header_is_validated() {
    use axum::extract::FromRequestParts;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    for (value, ok) in [
        ("short", false),
        ("valid_key-000000001", true),
        ("has space in the key!!", false),
    ] {
        let req = axum::http::Request::builder()
            .header("idempotency-key", value)
            .body(())
            .unwrap();
        let (mut parts, ()) = req.into_parts();
        let res = rt.block_on(IdempotencyKey::from_request_parts(&mut parts, &()));
        assert_eq!(res.is_ok(), ok, "{value}");
    }
}
