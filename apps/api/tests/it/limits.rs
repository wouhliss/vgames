//! A1-T16: every route is rate limited, and every API error is problem+json.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use crate::common;

use std::num::NonZeroU32;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use common::*;
use governor::Quota;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;
use vgames_api::{AppState, http::ratelimit::RateLimits};

/// Routes outside the OpenAPI document (docs, admin UI, storage URLs, dev sign-in, 404s).
const EXTRA_ROUTES: &[(&str, &str)] = &[
    ("GET", "/openapi.json"),
    ("GET", "/docs/"),
    ("GET", "/admin/"),
    ("GET", "/admin/users"),
    ("GET", "/_storage/packages/v1/x/y.pack"),
    ("PUT", "/_storage/_uploads/{id}"),
    ("GET", "/v1/auth/dev/fake-discord"),
    ("GET", "/no/such/route"),
    ("DELETE", "/v1/trust/bundle"),
];

fn operations() -> Vec<(String, String, bool)> {
    let doc = serde_json::to_value(vgames_api::http::openapi()).unwrap();
    let mut ops = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            let public = op["security"].as_array().is_some_and(|reqs| {
                reqs.iter()
                    .all(|r| r.get("bearerAuth").is_none() && r.get("cookieAuth").is_none())
            });
            ops.push((method.to_uppercase(), path.clone(), !public));
        }
    }
    ops.sort();
    ops
}

fn concrete(path: &str, garbage: bool) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let end = rest[start..].find('}').unwrap() + start;
        out.push_str(&if garbage {
            "not-a-valid-id%21".to_string()
        } else {
            match &rest[start + 1..end] {
                "pack_index" => "0".into(),
                "target" => "linux".into(),
                "platform" => "linux-x86_64".into(),
                "discord_id" => "123456789012".into(),
                _ => Uuid::now_v7().to_string(),
            }
        });
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

fn request(method: &str, uri: &str, auth: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(a) = auth {
        b = b.header("authorization", a);
    }
    if method == "GET" || method == "DELETE" {
        b.body(Body::empty()).unwrap()
    } else {
        b.header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap()
    }
}

/// A router whose every limit allows one request per hour.
fn tiny(pool: &PgPool, dist: &std::path::Path) -> Router {
    let config = config_with(&[("VGAMES_ADMIN_DIST", dist.to_str().unwrap())]);
    let state = AppState::new(config, pool.clone())
        .unwrap()
        .with_limits(RateLimits::with_quotas(|_| {
            Quota::per_hour(NonZeroU32::MIN)
        }))
        .ok()
        .unwrap();
    vgames_api::http::router(state)
}

async fn second_is_limited(
    pool: &PgPool,
    dist: &std::path::Path,
    method: &str,
    uri: &str,
    auth: Option<&str>,
) -> Result<(), String> {
    let app = tiny(pool, dist);
    let first = send(&app, request(method, uri, auth)).await.status();
    let second = send(&app, request(method, uri, auth)).await;
    if second.status() != StatusCode::TOO_MANY_REQUESTS {
        return Err(format!(
            "{method} {uri} (auth: {}): {first} then {}",
            auth.is_some(),
            second.status()
        ));
    }
    if second.headers().get("retry-after").is_none() {
        return Err(format!("{method} {uri}: 429 without Retry-After"));
    }
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn every_route_is_rate_limited(pool: PgPool) {
    let dist = tempfile::tempdir().unwrap();
    std::fs::write(dist.path().join("index.html"), "<!doctype html>").unwrap();
    let (_, _, owner) = seed_session(&pool, "owner").await;
    let junk = format!("Bearer vga_{}", "A".repeat(43));
    let mut failures = Vec::new();
    let mut ops = operations();
    assert!(ops.len() > 50, "{} operations", ops.len());
    ops.extend(
        EXTRA_ROUTES
            .iter()
            .map(|(m, p)| (m.to_string(), p.to_string(), false)),
    );

    for (method, template, authenticated) in &ops {
        let uri = concrete(template, false);
        // Anonymous, and with a token that does not exist: counted per IP.
        for auth in [None, Some(junk.as_str()), Some("Bearer nope")] {
            if let Err(e) = second_is_limited(&pool, dist.path(), method, &uri, auth).await {
                failures.push(e);
            }
        }
        // A real session: counted per user.
        if *authenticated {
            let auth = format!("Bearer {owner}");
            if let Err(e) = second_is_limited(&pool, dist.path(), method, &uri, Some(&auth)).await {
                failures.push(e);
            }
        }
    }
    assert!(
        failures.is_empty(),
        "routes without a limit:\n{}",
        failures.join("\n")
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn api_errors_are_problem_json_even_for_malformed_paths(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, owner) = seed_session(&pool, "owner").await;
    let auth = format!("Bearer {owner}");
    let mut failures = Vec::new();
    for (method, template, _) in operations() {
        if !template.contains('{') {
            continue;
        }
        let uri = concrete(&template, true);
        let resp = send(&app, request(&method, &uri, Some(&auth))).await;
        let status = resp.status();
        let ct = resp
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_string());
        // The realtime upgrade answers before path parsing; everything else must not match
        // a real resource.
        if status.is_success() || status.is_redirection() {
            failures.push(format!("{method} {uri}: {status}"));
        } else if ct.as_deref() != Some("application/problem+json") {
            let body = String::from_utf8_lossy(
                &http_body_util::BodyExt::collect(resp.into_body())
                    .await
                    .unwrap()
                    .to_bytes(),
            )
            .into_owned();
            failures.push(format!("{method} {uri}: {status} {ct:?} {body}"));
        } else {
            let body: Value = body_json(resp).await;
            assert!(body["code"].is_string());
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
