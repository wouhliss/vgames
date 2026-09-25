//! A4-T06: the game invite state machine (05-social §5, 04-database §3).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use crate::common::{self, publishing::json_req, *};
use crate::social_presence::{assert_quiet, befriend, instance, next_event};

use axum::{Router, http::StatusCode};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct User {
    id: Uuid,
    token: String,
}

async fn user(pool: &PgPool) -> User {
    let (id, _, token) = seed_session(pool, "user").await;
    User { id, token }
}

/// A published package with a linux release, visible in the catalog.
async fn game(pool: &PgPool, slug: &str, by: Uuid) -> Uuid {
    let package: Uuid = sqlx::query_scalar(
        "INSERT INTO packages (slug, title, status, created_by) VALUES ($1, $1, 'published', $2) RETURNING id",
    )
    .bind(slug)
    .bind(by)
    .fetch_one(pool)
    .await
    .unwrap();
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
    .bind(by)
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
    .bind(package)
    .bind(vec![1u8; 32])
    .bind(vec![2u8; 64])
    .bind(by)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO package_releases (package_id, platform, version_id, updated_by) VALUES ($1, 'linux-x86_64', $2, $3)")
        .bind(package)
        .bind(version)
        .bind(by)
        .execute(pool)
        .await
        .unwrap();
    package
}

async fn post(app: &Router, uri: &str, token: &str, body: &Value) -> (StatusCode, Value) {
    let resp = send(app, json_req("POST", uri, token, body)).await;
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn invite(app: &Router, from: &User, to: Uuid, package: Uuid) -> (StatusCode, Value) {
    post(
        app,
        "/v1/invites",
        &from.token,
        &json!({"to_user_id": to, "package_id": package}),
    )
    .await
}

#[derive(Clone, Copy, Debug)]
enum Action {
    Accept,
    Decline,
    Cancel,
    Installing,
    Ready,
    Joined,
    Failed,
}

async fn act(
    app: &Router,
    action: Action,
    id: &str,
    sender: &User,
    invitee: &User,
) -> (StatusCode, Value) {
    let (who, uri, body) = match action {
        Action::Accept => (invitee, "accept", Value::Null),
        Action::Decline => (invitee, "decline", Value::Null),
        Action::Cancel => (sender, "cancel", Value::Null),
        Action::Installing => (
            invitee,
            "status",
            json!({"state": "installing", "progress": 0.5}),
        ),
        Action::Ready => (invitee, "status", json!({"state": "ready"})),
        Action::Joined => (invitee, "status", json!({"state": "joined"})),
        Action::Failed => (
            invitee,
            "status",
            json!({"state": "failed", "failure_reason": "insufficient_space"}),
        ),
    };
    let uri = format!("/v1/invites/{id}/{uri}");
    if body.is_null() {
        let resp = send(app, bearer_request("POST", &uri, &who.token)).await;
        let status = resp.status();
        (status, body_json(resp).await)
    } else {
        post(app, &uri, &who.token, &body).await
    }
}

/// Inserts an invite directly in `state`.
async fn seed_invite(pool: &PgPool, from: Uuid, to: Uuid, package: Uuid, state: &str) -> String {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO game_invites (from_user_id, to_user_id, package_id, state, failure_reason, expires_at)
         VALUES ($1, $2, $3, $4, CASE WHEN $4 = 'failed' THEN 'install_failed' END, now() + interval '1 hour')
         RETURNING id",
    )
    .bind(from)
    .bind(to)
    .bind(package)
    .bind(state)
    .fetch_one(pool)
    .await
    .unwrap();
    id.to_string()
}

#[sqlx::test(migrations = "./migrations")]
async fn every_transition_is_allowed_or_refused_as_specified(pool: PgPool) {
    use Action::*;
    let app = common::app(pool.clone());
    let (a, b, c) = (user(&pool).await, user(&pool).await, user(&pool).await);
    befriend(&pool, a.id, b.id, "accepted").await;
    let package = game(&pool, "coop", a.id).await;

    // (action, states it applies to, resulting state)
    let table: [(Action, &[&str], &str); 7] = [
        (Accept, &["pending"], "accepted"),
        (Decline, &["pending"], "declined"),
        (
            Cancel,
            &["pending", "accepted", "installing", "ready"],
            "cancelled",
        ),
        (Installing, &["accepted", "installing"], "installing"),
        (Ready, &["accepted", "installing"], "ready"),
        (Joined, &["ready"], "joined"),
        (Failed, &["accepted", "installing"], "failed"),
    ];
    let all = [
        "pending",
        "accepted",
        "installing",
        "ready",
        "joined",
        "declined",
        "cancelled",
        "expired",
        "failed",
    ];
    let mut checked = 0;
    for (action, allowed, result) in table {
        for from in all {
            sqlx::query("DELETE FROM game_invites")
                .execute(&pool)
                .await
                .unwrap();
            let id = seed_invite(&pool, a.id, b.id, package, from).await;
            let (s, body) = act(&app, action, &id, &a, &b).await;
            if allowed.contains(&from) {
                assert_eq!(s, StatusCode::OK, "{action:?} from {from}: {body}");
                assert_eq!(body["state"], result, "{action:?} from {from}");
            } else {
                assert_eq!(
                    (s, body["code"].as_str()),
                    (StatusCode::CONFLICT, Some("invalid_transition")),
                    "{action:?} from {from}: {body}"
                );
                let state: String = sqlx::query_scalar("SELECT state FROM game_invites")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                assert_eq!(state, from, "a refused change must change nothing");
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 63);

    // The wrong party, or a stranger, sees no such invite.
    sqlx::query("DELETE FROM game_invites")
        .execute(&pool)
        .await
        .unwrap();
    let id = seed_invite(&pool, a.id, b.id, package, "pending").await;
    for (action, sender, invitee) in [
        (Accept, &b, &a),
        (Decline, &b, &a),
        (Cancel, &b, &a),
        (Accept, &a, &c),
        (Cancel, &c, &b),
    ] {
        let (s, _) = act(&app, action, &id, sender, invitee).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{action:?}");
    }
    let (s, _) = act(&app, Accept, &Uuid::now_v7().to_string(), &a, &b).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Accepting gives the invite 24 hours; failing records the reason; ready means 100 %.
    let (_, accepted) = act(&app, Accept, &id, &a, &b).await;
    let expires = time::OffsetDateTime::parse(
        accepted["expires_at"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    assert!(expires - time::OffsetDateTime::now_utc() > time::Duration::hours(23));
    let (_, ready) = act(&app, Ready, &id, &a, &b).await;
    assert_eq!(ready["progress"], 1.0);
    sqlx::query("DELETE FROM game_invites")
        .execute(&pool)
        .await
        .unwrap();
    let id = seed_invite(&pool, a.id, b.id, package, "accepted").await;
    let (_, failed) = act(&app, Failed, &id, &a, &b).await;
    assert_eq!(failed["failure_reason"], "insufficient_space");
}

#[sqlx::test(migrations = "./migrations")]
async fn status_reports_are_validated(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b) = (user(&pool).await, user(&pool).await);
    befriend(&pool, a.id, b.id, "accepted").await;
    let package = game(&pool, "coop", a.id).await;
    let id = seed_invite(&pool, a.id, b.id, package, "accepted").await;
    for bad in [
        json!({"state": "installing", "progress": 1.5}),
        json!({"state": "installing", "progress": -0.1}),
        json!({"state": "ready", "progress": 0.5}),
        json!({"state": "failed"}),
        json!({"state": "ready", "failure_reason": "install_failed"}),
        json!({"state": "failed", "failure_reason": "bored"}),
        json!({"state": "accepted"}),
        json!({"state": "ready", "extra": true}),
    ] {
        let (s, _) = post(&app, &format!("/v1/invites/{id}/status"), &b.token, &bad).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }
    // Installing without progress is fine.
    let (s, inv) = post(
        &app,
        &format!("/v1/invites/{id}/status"),
        &b.token,
        &json!({"state": "installing"}),
    )
    .await;
    assert_eq!(
        (s, inv["state"].as_str()),
        (StatusCode::OK, Some("installing"))
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn creating_invites(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b, c, d, e) = (
        user(&pool).await,
        user(&pool).await,
        user(&pool).await,
        user(&pool).await,
        user(&pool).await,
    );
    befriend(&pool, a.id, b.id, "accepted").await;
    befriend(&pool, a.id, c.id, "pending").await;
    befriend(&pool, a.id, e.id, "accepted").await;
    let package = game(&pool, "coop", a.id).await;

    let (s, inv) = post(
        &app,
        "/v1/invites",
        &a.token,
        &json!({"to_user_id": b.id, "package_id": package, "message": "  come play  "}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{inv}");
    assert_eq!(
        (inv["state"].as_str(), inv["message"].as_str()),
        (Some("pending"), Some("come play"))
    );
    assert_eq!(
        (inv["from"]["id"].as_str(), inv["to"]["id"].as_str()),
        (
            Some(a.id.to_string().as_str()),
            Some(b.id.to_string().as_str())
        )
    );
    assert_eq!(inv["package"]["id"], package.to_string());
    let expires = time::OffsetDateTime::parse(
        inv["expires_at"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let ttl = expires - time::OffsetDateTime::now_utc();
    assert!(ttl > time::Duration::minutes(9) && ttl <= time::Duration::minutes(10));

    // One active invite per sender, invitee and package.
    let (s, again) = invite(&app, &a, b.id, package).await;
    assert_eq!((s, &again["id"]), (StatusCode::OK, &inv["id"]));
    // Once it ends, a new one can be sent.
    let resp = send(
        &app,
        bearer_request(
            "POST",
            &format!("/v1/invites/{}/decline", inv["id"].as_str().unwrap()),
            &b.token,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let (s, fresh) = invite(&app, &a, b.id, package).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_ne!(fresh["id"], inv["id"]);

    // Both parties list it (with the declined one); others see nothing.
    for u in [&a, &b] {
        let resp = send(&app, bearer_request("GET", "/v1/invites", &u.token)).await;
        let list = body_json(resp).await;
        let states: Vec<&str> = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["state"].as_str().unwrap())
            .collect();
        assert_eq!(states, ["pending", "declined"]);
    }
    let resp = send(&app, bearer_request("GET", "/v1/invites", &d.token)).await;
    assert!(
        body_json(resp).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // Who can be invited: friends only; strangers and blocked users look unknown.
    assert_eq!(
        invite(&app, &a, a.id, package).await.0,
        StatusCode::BAD_REQUEST
    );
    let (s, err) = invite(&app, &a, c.id, package).await;
    assert_eq!(
        (s, err["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("not_allowed"))
    );
    assert_eq!(
        invite(&app, &a, d.id, package).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        invite(&app, &a, Uuid::now_v7(), package).await.0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("INSERT INTO user_blocks (blocker_id, blocked_id) VALUES ($1, $2)")
        .bind(e.id)
        .bind(a.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        invite(&app, &a, e.id, package).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        invite(&app, &e, a.id, package).await.0,
        StatusCode::NOT_FOUND
    );

    // What can be played: published packages with a release.
    let draft = game(&pool, "draft", a.id).await;
    sqlx::query("UPDATE packages SET status = 'draft' WHERE id = $1")
        .bind(draft)
        .execute(&pool)
        .await
        .unwrap();
    let unreleased: Uuid = sqlx::query_scalar("INSERT INTO packages (slug, title, status, created_by) VALUES ('empty', 'Empty', 'published', $1) RETURNING id")
        .bind(a.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    for p in [draft, unreleased, Uuid::now_v7()] {
        assert_eq!(invite(&app, &a, b.id, p).await.0, StatusCode::NOT_FOUND);
    }
    let (s, _) = post(
        &app,
        "/v1/invites",
        &a.token,
        &json!({"to_user_id": b.id, "package_id": package, "message": "x".repeat(201)}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "./migrations")]
async fn invite_creation_is_rate_limited(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b) = (user(&pool).await, user(&pool).await);
    befriend(&pool, a.id, b.id, "accepted").await;
    let package = game(&pool, "coop", a.id).await;
    for _ in 0..60 {
        let (s, _) = invite(&app, &a, b.id, package).await;
        assert!(s == StatusCode::CREATED || s == StatusCode::OK);
    }
    let (s, e) = invite(&app, &a, b.id, package).await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn events_reach_both_parties_and_progress_is_throttled(pool: PgPool) {
    let inst = instance(&pool).await;
    let app = inst.app();
    let (a, b) = (user(&pool).await, user(&pool).await);
    befriend(&pool, a.id, b.id, "accepted").await;
    let package = game(&pool, "coop", a.id).await;
    let mut wa = inst.connect(&a.token).await;
    let mut wb = inst.connect(&b.token).await;

    let (_, inv) = invite(&app, &a, b.id, package).await;
    let id = inv["id"].as_str().unwrap().to_string();
    for ws in [&mut wa, &mut wb] {
        let ev = next_event(ws).await;
        assert_eq!(
            (ev["type"].as_str(), ev["data"]["invite"]["id"].as_str()),
            (Some("invite.created"), Some(id.as_str()))
        );
    }
    act(&app, Action::Accept, &id, &a, &b).await;
    for ws in [&mut wa, &mut wb] {
        let ev = next_event(ws).await;
        assert_eq!(
            (ev["type"].as_str(), ev["data"]["invite"]["state"].as_str()),
            (Some("invite.updated"), Some("accepted"))
        );
    }

    let report = |p: f64| json!({"state": "installing", "progress": p});
    let uri = format!("/v1/invites/{id}/status");
    // Entering installing always publishes.
    post(&app, &uri, &b.token, &report(0.1)).await;
    let ev = next_event(&mut wa).await;
    assert_eq!(ev["data"]["invite"]["state"], "installing");
    assert!((ev["data"]["invite"]["progress"].as_f64().unwrap() - 0.1).abs() < 1e-6);
    // A report within 2 s is stored but not published.
    let (_, stored) = post(&app, &uri, &b.token, &report(0.2)).await;
    assert!((stored["progress"].as_f64().unwrap() - 0.2).abs() < 1e-6);
    assert_quiet(&inst.state, &mut wa, a.id).await;
    // After 2 s the next report is published.
    sqlx::query("UPDATE game_invites SET progress_published_at = now() - interval '3 seconds'")
        .execute(&pool)
        .await
        .unwrap();
    post(&app, &uri, &b.token, &report(0.3)).await;
    let ev = next_event(&mut wa).await;
    assert!((ev["data"]["invite"]["progress"].as_f64().unwrap() - 0.3).abs() < 1e-6);
    // State changes always publish.
    post(&app, &uri, &b.token, &json!({"state": "ready"})).await;
    assert_eq!(
        next_event(&mut wa).await["data"]["invite"]["state"],
        "ready"
    );
    post(&app, &uri, &b.token, &json!({"state": "joined"})).await;
    assert_eq!(
        next_event(&mut wa).await["data"]["invite"]["state"],
        "joined"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn expiry(pool: PgPool) {
    let inst = instance(&pool).await;
    let app = inst.app();
    let (a, b) = (user(&pool).await, user(&pool).await);
    befriend(&pool, a.id, b.id, "accepted").await;
    let package = game(&pool, "coop", a.id).await;
    let other = game(&pool, "other", a.id).await;
    let mut wa = inst.connect(&a.token).await;

    let (_, pending) = invite(&app, &a, b.id, package).await;
    let (_, accepted) = invite(&app, &a, b.id, other).await;
    next_event(&mut wa).await;
    next_event(&mut wa).await;
    act(
        &app,
        Action::Accept,
        accepted["id"].as_str().unwrap(),
        &a,
        &b,
    )
    .await;
    next_event(&mut wa).await;

    // The sweep expires overdue invites of any active state, and tells both parties.
    sqlx::query("UPDATE game_invites SET expires_at = now() - interval '1 second' WHERE id = $1")
        .bind(pending["id"].as_str().unwrap().parse::<Uuid>().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    vgames_api::social::sweep_once(&inst.state).await.unwrap();
    let ev = next_event(&mut wa).await;
    assert_eq!(
        (
            ev["data"]["invite"]["id"].as_str(),
            ev["data"]["invite"]["state"].as_str()
        ),
        (pending["id"].as_str(), Some("expired"))
    );
    let (s, e) = act(
        &app,
        Action::Accept,
        pending["id"].as_str().unwrap(),
        &a,
        &b,
    )
    .await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("invalid_transition"))
    );

    // Overdue invites also expire before any change, without waiting for the sweep.
    sqlx::query("UPDATE game_invites SET expires_at = now() - interval '1 second' WHERE id = $1")
        .bind(accepted["id"].as_str().unwrap().parse::<Uuid>().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let (s, _) = act(
        &app,
        Action::Ready,
        accepted["id"].as_str().unwrap(),
        &a,
        &b,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(
        next_event(&mut wa).await["data"]["invite"]["state"],
        "expired"
    );
    // A new invite for the same game can be sent.
    assert_eq!(invite(&app, &a, b.id, package).await.0, StatusCode::CREATED);
}

#[sqlx::test(migrations = "./migrations")]
async fn racing_requests_have_exactly_one_winner(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b) = (user(&pool).await, user(&pool).await);
    befriend(&pool, a.id, b.id, "accepted").await;
    let package = game(&pool, "coop", a.id).await;
    let a = std::sync::Arc::new(a);
    let b = std::sync::Arc::new(b);

    // Two terminal moves from the same state: exactly one succeeds, the state is the winner's.
    let pairs = [
        ("pending", Action::Decline, Action::Cancel),
        ("pending", Action::Accept, Action::Decline),
        ("ready", Action::Joined, Action::Cancel),
        ("installing", Action::Failed, Action::Cancel),
    ];
    for (start, x, y) in pairs {
        for round in 0..10 {
            sqlx::query("DELETE FROM game_invites")
                .execute(&pool)
                .await
                .unwrap();
            let id = seed_invite(&pool, a.id, b.id, package, start).await;
            let spawn = |action: Action| {
                let (app, id, a, b) = (app.clone(), id.clone(), a.clone(), b.clone());
                tokio::spawn(async move { act(&app, action, &id, &a, &b).await })
            };
            let (tx, ty) = (spawn(x), spawn(y));
            let (rx, ry) = (tx.await.unwrap(), ty.await.unwrap());
            let wins: Vec<&(StatusCode, Value)> = [&rx, &ry]
                .into_iter()
                .filter(|r| r.0 == StatusCode::OK)
                .collect();
            assert_eq!(
                wins.len(),
                1,
                "{start} {x:?}/{y:?} round {round}: {rx:?} {ry:?}"
            );
            let loser = if rx.0 == StatusCode::OK { &ry } else { &rx };
            assert_eq!(loser.0, StatusCode::CONFLICT);
            let state: String = sqlx::query_scalar("SELECT state FROM game_invites")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(state, wins[0].1["state"].as_str().unwrap());
        }
    }

    // Accept racing cancel: cancel always lands (it is allowed from pending and accepted),
    // and the invite always ends cancelled.
    for _ in 0..10 {
        sqlx::query("DELETE FROM game_invites")
            .execute(&pool)
            .await
            .unwrap();
        let id = seed_invite(&pool, a.id, b.id, package, "pending").await;
        let (app1, app2, id1, id2) = (app.clone(), app.clone(), id.clone(), id.clone());
        let (a1, b1, a2, b2) = (a.clone(), b.clone(), a.clone(), b.clone());
        let accept = tokio::spawn(async move { act(&app1, Action::Accept, &id1, &a1, &b1).await });
        let cancel = tokio::spawn(async move { act(&app2, Action::Cancel, &id2, &a2, &b2).await });
        let (ra, rc) = (accept.await.unwrap(), cancel.await.unwrap());
        assert_eq!(rc.0, StatusCode::OK);
        assert!(ra.0 == StatusCode::OK || ra.0 == StatusCode::CONFLICT);
        let state: String = sqlx::query_scalar("SELECT state FROM game_invites")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "cancelled");
    }
}
