//! A1-T14: admin server endpoints (users, allowlist, settings, jobs, audit log) and the
//! authorization matrix over every admin route.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use crate::common;

use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use common::*;
use futures_util::StreamExt;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_tungstenite::tungstenite::{Message, protocol::frame::coding::CloseCode};
use uuid::Uuid;

use crate::realtime::{connect, instance, next_event, ticket};

/// Admin operations only an owner may call.
const OWNER_ONLY: &[&str] = &["PATCH /v1/admin/settings", "POST /v1/admin/trust/bundles"];

fn req(method: &str, uri: &str, token: Option<&str>, body: Option<&Value>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(v).unwrap()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<&Value>,
) -> (StatusCode, Value, axum::http::HeaderMap) {
    let resp = send(app, req(method, uri, Some(token), body)).await;
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            panic!(
                "{method} {uri}: {status} is not JSON: {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, v, headers)
}

/// Every `/v1/admin/*` operation of an OpenAPI document.
fn admin_operations_of(doc: &Value) -> Vec<(String, String)> {
    let mut ops = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        if !path.starts_with("/v1/admin/") {
            continue;
        }
        for method in ["get", "post", "put", "patch", "delete"] {
            if item.get(method).is_some() {
                ops.push((method.to_uppercase(), path.clone()));
            }
        }
    }
    ops.sort();
    ops
}

/// The served admin operations, checked to be exactly the contract's.
fn admin_operations() -> Vec<(String, String)> {
    let served = admin_operations_of(&serde_json::to_value(vgames_api::http::openapi()).unwrap());
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../openapi/openapi.yaml");
    let contract: Value = serde_yaml_ng::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(
        served,
        admin_operations_of(&contract),
        "every contract admin route is served"
    );
    served
}

fn concrete(path: &str) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let end = rest[start..].find('}').unwrap() + start;
        out.push_str(&match &rest[start + 1..end] {
            "pack_index" => "0".to_string(),
            "target" => "linux".to_string(),
            "discord_id" => "123456789012".to_string(),
            _ => Uuid::now_v7().to_string(),
        });
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

async fn disabled_session(pool: &PgPool) -> String {
    let (user, _, token) = seed_session(pool, "admin").await;
    sqlx::query("UPDATE users SET disabled_at = now() WHERE id = $1")
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
    token
}

#[sqlx::test(migrations = "./migrations")]
async fn authorization_matrix_covers_every_admin_route(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, user) = seed_session(&pool, "user").await;
    let (_, _, admin) = seed_session(&pool, "admin").await;
    let (_, _, owner) = seed_session(&pool, "owner").await;
    let ops = admin_operations();
    assert_eq!(ops.len(), 34);

    for (method, path) in &ops {
        let op = format!("{method} {path}");
        let uri = concrete(path);
        let body = (method != "GET" && method != "DELETE").then(|| json!({}));

        let resp = send(&app, req(method, &uri, None, body.as_ref())).await;
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{op} without credentials"
        );

        let (status, problem, _) = call(&app, method, &uri, &user, body.as_ref()).await;
        assert_eq!(
            (status, problem["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("forbidden")),
            "{op} as user"
        );

        let disabled = disabled_session(&pool).await;
        let (status, problem, _) = call(&app, method, &uri, &disabled, body.as_ref()).await;
        assert_eq!(
            (status, problem["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("user_disabled")),
            "{op} as a disabled admin"
        );

        let (status, problem, _) = call(&app, method, &uri, &admin, body.as_ref()).await;
        if OWNER_ONLY.contains(&op.as_str()) {
            assert_eq!(
                (status, problem["code"].as_str()),
                (StatusCode::FORBIDDEN, Some("forbidden")),
                "{op} as admin"
            );
        } else {
            assert!(
                status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
                "{op} as admin: {status} {problem}"
            );
        }

        let (status, problem, _) = call(&app, method, &uri, &owner, body.as_ref()).await;
        assert!(
            status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
            "{op} as owner: {status} {problem}"
        );
    }
}

async fn audit_actions(pool: &PgPool, target_id: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM audit_log WHERE target_id = $1 ORDER BY id")
        .bind(target_id)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn rename(pool: &PgPool, id: Uuid, username: &str) {
    sqlx::query("UPDATE users SET username = $2 WHERE id = $1")
        .bind(id)
        .bind(username)
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn users_can_be_listed_and_searched(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, admin) = seed_session(&pool, "admin").await;
    let mut ids = Vec::new();
    for name in ["alice_100%", "bob", "carol"] {
        let (id, _, _) = seed_session(&pool, "user").await;
        rename(&pool, id, name).await;
        ids.push(id);
    }
    let discord: String = sqlx::query_scalar("SELECT discord_id FROM users WHERE id = $1")
        .bind(ids[1])
        .fetch_one(&pool)
        .await
        .unwrap();

    let (status, page, _) = call(&app, "GET", "/v1/admin/users?limit=2", &admin, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["items"][0]["id"], ids[2].to_string(), "newest first");
    let cursor = page["next_cursor"].as_str().unwrap();
    let (_, page2, _) = call(
        &app,
        "GET",
        &format!("/v1/admin/users?limit=2&cursor={cursor}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(page2["items"].as_array().unwrap().len(), 2);
    assert!(page2.get("next_cursor").is_none());
    // A cursor from another filter is refused.
    let (status, problem, _) = call(
        &app,
        "GET",
        &format!("/v1/admin/users?limit=2&role=user&cursor={cursor}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(problem["code"], "invalid_cursor");

    let names = |v: &Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["username"].as_str().unwrap().to_string())
            .collect()
    };
    let (_, found, _) = call(&app, "GET", "/v1/admin/users?q=ALICE", &admin, None).await;
    assert_eq!(names(&found), vec!["alice_100%"]);
    // LIKE wildcards in the query are literal.
    let (_, found, _) = call(&app, "GET", "/v1/admin/users?q=%25", &admin, None).await;
    assert_eq!(names(&found), vec!["alice_100%"]);
    let (_, found, _) = call(
        &app,
        "GET",
        &format!("/v1/admin/users?q={discord}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(names(&found), vec!["bob"]);
    let (_, found, _) = call(&app, "GET", "/v1/admin/users?role=admin", &admin, None).await;
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    assert_eq!(found["items"][0]["role"], "admin");
    assert!(found["items"][0]["discord_id"].is_string());

    let (status, _, _) = call(&app, "GET", "/v1/admin/users?role=root", &admin, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = call(
        &app,
        "GET",
        &format!("/v1/admin/users?q={}", "x".repeat(65)),
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "./migrations")]
async fn role_changes_follow_the_owner_rules(pool: PgPool) {
    let app = app(pool.clone());
    let (owner_id, _, owner) = seed_session(&pool, "owner").await;
    let (admin_id, _, admin) = seed_session(&pool, "admin").await;
    let (user_id, _, _) = seed_session(&pool, "user").await;
    let patch = |id: Uuid| format!("/v1/admin/users/{id}");

    // Admins cannot change roles, not even to promote someone.
    let (status, problem, _) = call(
        &app,
        "PATCH",
        &patch(user_id),
        &admin,
        Some(&json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );

    let (status, body, _) = call(
        &app,
        "PATCH",
        &patch(user_id),
        &owner,
        Some(&json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["role"], "admin");
    assert_eq!(
        audit_actions(&pool, &user_id.to_string()).await,
        vec!["user.role_change"]
    );

    // The last owner cannot step down.
    let (status, problem, _) = call(
        &app,
        "PATCH",
        &patch(owner_id),
        &owner,
        Some(&json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::CONFLICT, Some("last_owner"))
    );

    // With a second owner, it can.
    let (status, _, _) = call(
        &app,
        "PATCH",
        &patch(admin_id),
        &owner,
        Some(&json!({ "role": "owner" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body, _) = call(
        &app,
        "PATCH",
        &patch(owner_id),
        &owner,
        Some(&json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["role"], "admin");
    // The caller is now an admin: the change applies to its next request.
    let (status, _, _) = call(
        &app,
        "PATCH",
        &patch(user_id),
        &owner,
        Some(&json!({ "role": "user" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // No-op patches and unknown users.
    let (status, _, _) = call(&app, "PATCH", &patch(user_id), &admin, Some(&json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(
        &app,
        "PATCH",
        &patch(Uuid::now_v7()),
        &admin,
        Some(&json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, problem, _) = call(
        &app,
        "PATCH",
        &patch(user_id),
        &admin,
        Some(&json!({ "roles": "x" })),
    )
    .await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("unknown_field"))
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn two_owners_demoting_each_other_leave_one_owner(pool: PgPool) {
    let app = app(pool.clone());
    let (a, _, a_token) = seed_session(&pool, "owner").await;
    let (b, _, b_token) = seed_session(&pool, "owner").await;
    let body = json!({ "role": "admin" });
    let (uri_b, uri_a) = (
        format!("/v1/admin/users/{b}"),
        format!("/v1/admin/users/{a}"),
    );
    let (ra, rb) = tokio::join!(
        call(&app, "PATCH", &uri_b, &a_token, Some(&body)),
        call(&app, "PATCH", &uri_a, &b_token, Some(&body)),
    );
    // The loser gets 409 last_owner when both hold the owner lock in turn, or 403 when its
    // own demotion committed before its request was authorized. Either way one owner stays.
    let mut statuses = [ra.0, rb.0];
    statuses.sort();
    assert_eq!(statuses[0], StatusCode::OK, "{} / {}", ra.1, rb.1);
    assert!(
        [StatusCode::FORBIDDEN, StatusCode::CONFLICT].contains(&statuses[1]),
        "{} / {}",
        ra.1,
        rb.1
    );
    let owners: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE role = 'owner'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(owners, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn disabling_revokes_sessions_and_closes_sockets(pool: PgPool) {
    let inst = instance(&pool).await;
    let app = vgames_api::http::router(inst.state.clone());
    let (owner_id, _, owner) = seed_session(&pool, "owner").await;
    let (admin_id, _, admin) = seed_session(&pool, "admin").await;
    let (other_admin, _, _) = seed_session(&pool, "admin").await;
    let (user_id, session, user) = seed_session(&pool, "user").await;
    let t = ticket(&inst, &user).await;
    let mut ws = connect(&inst, &t).await.unwrap();
    assert_eq!(next_event(&mut ws).await["type"], "hello");
    let patch = |id: Uuid| format!("/v1/admin/users/{id}");

    let (status, problem, _) = call(
        &app,
        "PATCH",
        &patch(user_id),
        &admin,
        Some(&json!({ "disabled_reason": "spam" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "reason needs disabled: true"
    );
    assert_eq!(problem["errors"][0]["field"], "disabled_reason");

    let (status, body, _) = call(
        &app,
        "PATCH",
        &patch(user_id),
        &admin,
        Some(&json!({ "disabled": true, "disabled_reason": "spam" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["disabled_at"].is_string());
    assert_eq!(body["disabled_reason"], "spam");

    let ev = next_event(&mut ws).await;
    assert_eq!(
        (ev["type"].as_str(), ev["data"]["reason"].as_str()),
        (Some("session.revoked"), Some("user_disabled"))
    );
    match tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .unwrap()
    {
        Some(Ok(Message::Close(Some(frame)))) => assert_eq!(frame.code, CloseCode::from(4001)),
        other => panic!("expected close, got {other:?}"),
    }
    let reason: Option<String> =
        sqlx::query_scalar("SELECT revoked_reason FROM sessions WHERE id = $1")
            .bind(session)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reason.as_deref(), Some("user_disabled"));
    let resp = send(&app, bearer_request("GET", "/v1/me", &user)).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Admins cannot disable admins or owners; nobody can disable themselves.
    for target in [other_admin, owner_id] {
        let (status, _, _) = call(
            &app,
            "PATCH",
            &patch(target),
            &admin,
            Some(&json!({ "disabled": true })),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let (status, problem, _) = call(
        &app,
        "PATCH",
        &patch(admin_id),
        &admin,
        Some(&json!({ "disabled": true })),
    )
    .await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("cannot_disable_self"))
    );
    let (status, problem, _) = call(
        &app,
        "PATCH",
        &patch(owner_id),
        &owner,
        Some(&json!({ "disabled": true })),
    )
    .await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("cannot_disable_self"))
    );
    // Owners can disable admins.
    let (status, _, _) = call(
        &app,
        "PATCH",
        &patch(other_admin),
        &owner,
        Some(&json!({ "disabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Re-enabling clears the reason; the user signs in again to get a session.
    let (status, body, _) = call(
        &app,
        "PATCH",
        &patch(user_id),
        &admin,
        Some(&json!({ "disabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("disabled_at").is_none() && body.get("disabled_reason").is_none());
    assert_eq!(
        audit_actions(&pool, &user_id.to_string()).await,
        vec!["user.disable", "user.enable"]
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn allowlist_crud(pool: PgPool) {
    let app = app(pool.clone());
    let (admin_id, _, admin) = seed_session(&pool, "admin").await;

    let (status, body, _) = call(
        &app,
        "POST",
        "/v1/admin/allowlist",
        &admin,
        Some(&json!({ "discord_id": "80351110224678912", "note": " friend of alice " })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["note"], "friend of alice");
    assert_eq!(body["added_by"]["id"], admin_id.to_string());

    let (status, problem, _) = call(
        &app,
        "POST",
        "/v1/admin/allowlist",
        &admin,
        Some(&json!({ "discord_id": "80351110224678912" })),
    )
    .await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::CONFLICT, Some("already_allowlisted"))
    );
    for bad in [
        json!({ "discord_id": "12ab5" }),
        json!({ "discord_id": "1234" }),
        json!({ "discord_id": "123456", "note": "x".repeat(201) }),
    ] {
        let (status, _, _) = call(&app, "POST", "/v1/admin/allowlist", &admin, Some(&bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }

    let (status, list, _) = call(&app, "GET", "/v1/admin/allowlist", &admin, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    let (status, _, _) = call(
        &app,
        "DELETE",
        "/v1/admin/allowlist/80351110224678912",
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = call(
        &app,
        "DELETE",
        "/v1/admin/allowlist/80351110224678912",
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(
        &app,
        "DELETE",
        "/v1/admin/allowlist/not-an-id",
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        audit_actions(&pool, "80351110224678912").await,
        vec!["allowlist.add", "allowlist.remove"]
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn settings_need_the_owner_and_if_match(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, admin) = seed_session(&pool, "admin").await;
    let (_, _, owner) = seed_session(&pool, "owner").await;

    let (status, body, headers) = call(&app, "GET", "/v1/admin/settings", &admin, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "registration_mode": "allowlist", "name": test_config().server_name })
    );
    let etag = headers["etag"].to_str().unwrap().to_string();

    let patch =
        json!({ "registration_mode": "open", "name": "  Friday Night Games ", "motd": "Welcome!" });
    let send_patch = |token: &str, if_match: Option<&str>, body: &Value| {
        let mut r = req("PATCH", "/v1/admin/settings", Some(token), Some(body));
        r.headers_mut().insert(
            "content-type",
            "application/merge-patch+json".parse().unwrap(),
        );
        if let Some(m) = if_match {
            r.headers_mut().insert("if-match", m.parse().unwrap());
        }
        send(&app, r)
    };
    assert_eq!(
        send_patch(&admin, Some(&etag), &patch).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send_patch(&owner, None, &patch).await.status(),
        StatusCode::PRECONDITION_REQUIRED
    );
    assert_eq!(
        send_patch(&owner, Some("W/\"1\""), &patch).await.status(),
        StatusCode::PRECONDITION_FAILED
    );
    let bad = send_patch(&owner, Some(&etag), &json!({ "name": "   " })).await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    let resp = send_patch(&owner, Some(&etag), &patch).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let new_etag = resp.headers()["etag"].to_str().unwrap().to_string();
    assert_ne!(new_etag, etag);
    assert_eq!(
        body_json(resp).await,
        json!({ "registration_mode": "open", "name": "Friday Night Games", "motd": "Welcome!" })
    );
    // The stale ETag no longer matches.
    assert_eq!(
        send_patch(&owner, Some(&etag), &json!({ "motd": "" }))
            .await
            .status(),
        StatusCode::PRECONDITION_FAILED
    );

    // Discovery serves the new values.
    let info = body_json(send(&app, get_req("/.well-known/vgames.json")).await).await;
    assert_eq!(info["name"], "Friday Night Games");
    assert_eq!(info["motd"], "Welcome!");
    assert_eq!(info["registration_mode"], "open");

    // An empty MOTD clears it.
    let resp = send_patch(&owner, Some(&new_etag), &json!({ "motd": "" })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(body_json(resp).await.get("motd").is_none());
    let changes: Vec<Value> = sqlx::query_scalar(
        "SELECT details FROM audit_log WHERE action = 'settings.update' ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0]["server.name"], "Friday Night Games");
}

async fn job(pool: &PgPool, kind: &str, state: &str, dedupe: Option<&str>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO jobs (kind, state, attempts, max_attempts, last_error, dedupe_key, finished_at)
         VALUES ($1, $2, 5, 5, 'boom', $3, CASE WHEN $2 IN ('dead', 'succeeded', 'failed') THEN now() END)
         RETURNING id",
    )
    .bind(kind)
    .bind(state)
    .bind(dedupe)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn jobs_can_be_listed_and_retried(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, admin) = seed_session(&pool, "admin").await;
    let dead = job(&pool, "metadata.fetch", "dead", Some("metadata.fetch:x")).await;
    let done = job(&pool, "metadata.fetch", "succeeded", None).await;
    let failed = job(&pool, "saves.gc", "failed", None).await;
    let blocked = job(&pool, "metadata.fetch", "dead", Some("metadata.fetch:y")).await;
    job(&pool, "metadata.fetch", "queued", Some("metadata.fetch:y")).await;

    let (_, page, _) = call(&app, "GET", "/v1/admin/jobs?state=dead", &admin, None).await;
    let ids: Vec<&str> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![blocked.to_string(), dead.to_string()]);
    let (_, page, _) = call(&app, "GET", "/v1/admin/jobs?kind=saves.gc", &admin, None).await;
    assert_eq!(page["items"][0]["id"], failed.to_string());
    assert_eq!(page["items"][0]["last_error"], "boom");
    let (status, _, _) = call(&app, "GET", "/v1/admin/jobs?state=exploded", &admin, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body, _) = call(
        &app,
        "POST",
        &format!("/v1/admin/jobs/{dead}/retry"),
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (body["state"].as_str(), body["attempts"].as_i64()),
        (Some("queued"), Some(0))
    );
    let (status, _, _) = call(
        &app,
        "POST",
        &format!("/v1/admin/jobs/{failed}/retry"),
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    for (id, code) in [
        (done, "job_not_retryable"),
        (dead, "job_not_retryable"),
        (blocked, "job_already_queued"),
    ] {
        let (status, problem, _) = call(
            &app,
            "POST",
            &format!("/v1/admin/jobs/{id}/retry"),
            &admin,
            None,
        )
        .await;
        assert_eq!(
            (status, problem["code"].as_str()),
            (StatusCode::CONFLICT, Some(code))
        );
    }
    let (status, _, _) = call(
        &app,
        "POST",
        &format!("/v1/admin/jobs/{}/retry", Uuid::now_v7()),
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        audit_actions(&pool, &dead.to_string()).await,
        vec!["job.retry"]
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn audit_log_filters_and_pages(pool: PgPool) {
    let app = app(pool.clone());
    let (admin_id, _, admin) = seed_session(&pool, "admin").await;
    let (other_id, _, other) = seed_session(&pool, "admin").await;
    for id in ["11111111", "22222222", "33333333"] {
        let (status, _, _) = call(
            &app,
            "POST",
            "/v1/admin/allowlist",
            &admin,
            Some(&json!({ "discord_id": id })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (status, _, _) = call(&app, "DELETE", "/v1/admin/allowlist/11111111", &other, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, page, _) = call(&app, "GET", "/v1/admin/audit-log?limit=2", &admin, None).await;
    assert_eq!(status, StatusCode::OK);
    let first = &page["items"][0];
    assert_eq!(first["action"], "allowlist.remove");
    assert_eq!(first["actor"]["id"], other_id.to_string());
    assert_eq!(first["target_type"], "discord_account");
    assert!(first["details"].is_object());
    let cursor = page["next_cursor"].as_str().unwrap();
    let (_, page2, _) = call(
        &app,
        "GET",
        &format!("/v1/admin/audit-log?limit=2&cursor={cursor}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(page2["items"].as_array().unwrap().len(), 2);
    assert!(page2.get("next_cursor").is_none());

    let count = |v: &Value| v["items"].as_array().unwrap().len();
    let (_, by_actor, _) = call(
        &app,
        "GET",
        &format!("/v1/admin/audit-log?actor_user_id={admin_id}"),
        &admin,
        None,
    )
    .await;
    assert_eq!(count(&by_actor), 3);
    let (_, by_action, _) = call(
        &app,
        "GET",
        "/v1/admin/audit-log?action=allowlist.add",
        &admin,
        None,
    )
    .await;
    assert_eq!(count(&by_action), 3);
    let (_, by_target, _) = call(
        &app,
        "GET",
        "/v1/admin/audit-log?target_type=discord_account&target_id=11111111",
        &admin,
        None,
    )
    .await;
    assert_eq!(count(&by_target), 2);
    let (_, future, _) = call(
        &app,
        "GET",
        "/v1/admin/audit-log?since=2999-01-01T00:00:00Z",
        &admin,
        None,
    )
    .await;
    assert_eq!(count(&future), 0);
    let (_, past, _) = call(
        &app,
        "GET",
        "/v1/admin/audit-log?until=2000-01-01T00:00:00Z",
        &admin,
        None,
    )
    .await;
    assert_eq!(count(&past), 0);
    let (_, all, _) = call(
        &app,
        "GET",
        "/v1/admin/audit-log?since=2000-01-01T00:00:00Z",
        &admin,
        None,
    )
    .await;
    assert_eq!(count(&all), 4);
    let (status, _, _) = call(
        &app,
        "GET",
        "/v1/admin/audit-log?since=yesterday",
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
