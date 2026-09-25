//! A4-T03: friends, friend codes, blocks and profiles (05-social §2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use crate::common::{self, publishing::json_req, *};

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

/// What a client can tell apart in a problem (`instance` and `request_id` vary per request).
fn problem(v: &(StatusCode, Value)) -> (StatusCode, Value, Value, Value) {
    (
        v.0,
        v.1["type"].clone(),
        v.1["code"].clone(),
        v.1["title"].clone(),
    )
}

async fn call(app: &Router, method: &str, uri: &str, u: &User) -> (StatusCode, Value) {
    let resp = send(app, bearer_request(method, uri, &u.token)).await;
    let status = resp.status();
    let body = if status == StatusCode::NO_CONTENT {
        Value::Null
    } else {
        body_json(resp).await
    };
    (status, body)
}

async fn post(app: &Router, uri: &str, u: &User, body: Value) -> (StatusCode, Value) {
    let resp = send(app, json_req("POST", uri, &u.token, &body)).await;
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn new_code(app: &Router, u: &User) -> String {
    let (s, b) = call(app, "POST", "/v1/friend-codes", u).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    b["code"].as_str().unwrap().to_string()
}

async fn redeem(app: &Router, u: &User, code: &str) -> (StatusCode, Value) {
    post(app, "/v1/friends/requests", u, json!({"friend_code": code})).await
}

async fn request_user(app: &Router, u: &User, to: Uuid) -> (StatusCode, Value) {
    post(app, "/v1/friends/requests", u, json!({"user_id": to})).await
}

/// Makes `a` and `b` accepted friends through a friend code.
async fn befriend(app: &Router, a: &User, b: &User) {
    let code = new_code(app, a).await;
    assert_eq!(redeem(app, b, &code).await.0, StatusCode::CREATED);
    let (s, body) = call(app, "POST", &format!("/v1/friends/{}/accept", b.id), a).await;
    assert_eq!(s, StatusCode::OK, "{body}");
}

async fn list(app: &Router, u: &User) -> Value {
    let (s, b) = call(app, "GET", "/v1/friends", u).await;
    assert_eq!(s, StatusCode::OK);
    b
}

fn ids(list: &Value, key: &str) -> Vec<String> {
    list[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["user"]["id"].as_str().unwrap().to_string())
        .collect()
}

async fn share_conversation(pool: &PgPool, a: Uuid, b: Uuid) {
    let conv: Uuid = sqlx::query_scalar(
        "INSERT INTO conversations (kind, created_by) VALUES ('party', $1) RETURNING id",
    )
    .bind(a)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO conversation_members (conversation_id, user_id) VALUES ($1, $2), ($1, $3)",
    )
    .bind(conv)
    .bind(a)
    .bind(b)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn friend_code_request_accept_and_list(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b, c) = (user(&pool).await, user(&pool).await, user(&pool).await);

    let (s, code) = call(&app, "POST", "/v1/friend-codes", &a).await;
    assert_eq!(s, StatusCode::CREATED);
    let text = code["code"].as_str().unwrap();
    assert!(vgames_proto::social::is_friend_code(text), "{text}");
    let expires: time::OffsetDateTime = time::OffsetDateTime::parse(
        code["expires_at"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let ttl = expires - time::OffsetDateTime::now_utc();
    assert!(ttl > time::Duration::minutes(14) && ttl <= time::Duration::minutes(15));

    let (s, friend) = redeem(&app, &b, text).await;
    assert_eq!(s, StatusCode::CREATED, "{friend}");
    assert_eq!(friend["state"], "outgoing");
    assert_eq!(friend["user"]["id"], a.id.to_string());

    // Single use: nobody else can redeem it, with the same answer as an unknown code.
    let (s, err) = redeem(&app, &c, text).await;
    assert_eq!(
        (s, err["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("friend_code_invalid"))
    );
    let (s, err2) = redeem(&app, &c, "ZZZZZZZZ").await;
    assert_eq!(
        (s, err2["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("friend_code_invalid"))
    );

    assert_eq!(
        ids(&list(&app, &a).await, "incoming"),
        vec![b.id.to_string()]
    );
    assert_eq!(
        ids(&list(&app, &b).await, "outgoing"),
        vec![a.id.to_string()]
    );

    // Only the recipient can accept.
    let (s, _) = call(&app, "POST", &format!("/v1/friends/{}/accept", a.id), &b).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, accepted) = call(&app, "POST", &format!("/v1/friends/{}/accept", b.id), &a).await;
    assert_eq!(s, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["state"], "accepted");
    assert_eq!(accepted["presence"]["status"], "offline");
    let (s, again) = call(&app, "POST", &format!("/v1/friends/{}/accept", b.id), &a).await;
    assert_eq!(
        (s, again["code"].as_str()),
        (StatusCode::CONFLICT, Some("already_friends"))
    );

    let la = list(&app, &a).await;
    assert_eq!(ids(&la, "friends"), vec![b.id.to_string()]);
    assert!(la["incoming"].as_array().unwrap().is_empty());
    assert_eq!(
        ids(&list(&app, &b).await, "friends"),
        vec![a.id.to_string()]
    );

    // A request between friends changes nothing.
    let (s, same) = request_user(&app, &b, a.id).await;
    assert_eq!(
        (s, same["state"].as_str()),
        (StatusCode::CREATED, Some("accepted"))
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn friend_code_edge_cases(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b) = (user(&pool).await, user(&pool).await);

    let own = new_code(&app, &a).await;
    let (s, err) = redeem(&app, &a, &own).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{err}");
    assert_eq!(err["errors"][0]["field"], "friend_code");

    let expired = new_code(&app, &a).await;
    sqlx::query("UPDATE friend_codes SET expires_at = now() - interval '1 second' WHERE code = $1")
        .bind(&expired)
        .execute(&pool)
        .await
        .unwrap();
    let (s, err) = redeem(&app, &b, &expired).await;
    assert_eq!(
        (s, err["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("friend_code_invalid"))
    );

    // Lowercase or ambiguous characters are not canonical (the launcher normalizes).
    for bad in ["abcdefgh", "ABCDEFGI", "ABC", "ABCDEFGHJ"] {
        let (s, err) = redeem(&app, &b, bad).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}: {err}");
    }
    // Exactly one of user_id / friend_code.
    let (s, _) = post(
        &app,
        "/v1/friends/requests",
        &b,
        json!({"user_id": a.id, "friend_code": own}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = post(&app, "/v1/friends/requests", &b, json!({})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = request_user(&app, &a, a.id).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // social.sweep deletes codes a day after they expire.
    sqlx::query("UPDATE friend_codes SET expires_at = now() - interval '2 days' WHERE code = $1")
        .bind(&expired)
        .execute(&pool)
        .await
        .unwrap();
    vgames_api::social::sweep_once(&common::state(pool.clone()))
        .await
        .unwrap();
    let left: Vec<String> = sqlx::query_scalar("SELECT code FROM friend_codes ORDER BY code")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(left, vec![own]);
}

#[sqlx::test(migrations = "./migrations")]
async fn requests_by_user_id_need_visibility_and_mutual_requests_accept(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b, stranger) = (user(&pool).await, user(&pool).await, user(&pool).await);

    // A stranger's id answers exactly like an unknown id.
    let stranger_err = request_user(&app, &a, stranger.id).await;
    let unknown_err = request_user(&app, &a, Uuid::now_v7()).await;
    assert_eq!(stranger_err.0, StatusCode::NOT_FOUND);
    assert_eq!(problem(&stranger_err), problem(&unknown_err));

    share_conversation(&pool, a.id, b.id).await;
    let (s, f) = request_user(&app, &a, b.id).await;
    assert_eq!(
        (s, f["state"].as_str()),
        (StatusCode::CREATED, Some("outgoing"))
    );
    // Repeating it keeps the pending request.
    let (s, f) = request_user(&app, &a, b.id).await;
    assert_eq!(
        (s, f["state"].as_str()),
        (StatusCode::CREATED, Some("outgoing"))
    );
    // B asking back accepts it.
    let (s, f) = request_user(&app, &b, a.id).await;
    assert_eq!(
        (s, f["state"].as_str()),
        (StatusCode::CREATED, Some("accepted"))
    );
    assert_eq!(
        ids(&list(&app, &a).await, "friends"),
        vec![b.id.to_string()]
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn decline_and_remove(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b) = (user(&pool).await, user(&pool).await);

    let code = new_code(&app, &a).await;
    redeem(&app, &b, &code).await;
    // The requester cannot decline their own request; the recipient can.
    let (s, _) = call(&app, "POST", &format!("/v1/friends/{}/decline", a.id), &b).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&app, "POST", &format!("/v1/friends/{}/decline", b.id), &a).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(&app, "POST", &format!("/v1/friends/{}/decline", b.id), &a).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(ids(&list(&app, &b).await, "outgoing").is_empty());

    // Cancel an outgoing request.
    let code = new_code(&app, &a).await;
    redeem(&app, &b, &code).await;
    let (s, _) = call(&app, "DELETE", &format!("/v1/friends/{}", a.id), &b).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(ids(&list(&app, &a).await, "incoming").is_empty());

    befriend(&app, &a, &b).await;
    let (s, _) = call(&app, "DELETE", &format!("/v1/friends/{}", a.id), &b).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(ids(&list(&app, &a).await, "friends").is_empty());
    assert!(ids(&list(&app, &b).await, "friends").is_empty());
    let (s, _) = call(&app, "DELETE", &format!("/v1/friends/{}", a.id), &b).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "./migrations")]
async fn blocks_are_symmetric_and_look_like_unknown_users(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b, c) = (user(&pool).await, user(&pool).await, user(&pool).await);
    befriend(&app, &a, &b).await;
    let profile = |u: Uuid| format!("/v1/users/{u}");
    assert_eq!(
        call(&app, "GET", &profile(a.id), &b).await.0,
        StatusCode::OK
    );

    let (s, _) = call(&app, "POST", &format!("/v1/blocks/{}", b.id), &a).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    // Blocking again is a no-op.
    assert_eq!(
        call(&app, "POST", &format!("/v1/blocks/{}", b.id), &a)
            .await
            .0,
        StatusCode::NO_CONTENT
    );

    // The friendship is gone on both sides.
    assert!(ids(&list(&app, &a).await, "friends").is_empty());
    assert!(ids(&list(&app, &b).await, "friends").is_empty());

    // Both directions: profiles, requests and codes answer like an unknown user.
    let unknown = problem(&call(&app, "GET", &profile(Uuid::now_v7()), &b).await);
    assert_eq!(unknown.0, StatusCode::NOT_FOUND);
    assert_eq!(unknown.2, "not_found");
    for (from, to) in [(&a, &b), (&b, &a)] {
        assert_eq!(
            problem(&call(&app, "GET", &profile(to.id), from).await),
            unknown
        );
        assert_eq!(problem(&request_user(&app, from, to.id).await), unknown);
        let code = new_code(&app, to).await;
        let (s, e) = redeem(&app, from, &code).await;
        assert_eq!(
            (s, e["code"].as_str()),
            (StatusCode::NOT_FOUND, Some("friend_code_invalid"))
        );
        // The code stays usable for anyone else.
        let (s, _) = redeem(&app, &c, &code).await;
        assert_eq!(s, StatusCode::CREATED);
        call(&app, "DELETE", &format!("/v1/friends/{}", to.id), &c).await;
    }
    // The blocked user cannot block back to probe the block, and gets the same 404.
    assert_eq!(
        problem(&call(&app, "POST", &format!("/v1/blocks/{}", a.id), &b).await),
        unknown
    );

    // Only the blocker can unblock.
    assert_eq!(
        call(&app, "DELETE", &format!("/v1/blocks/{}", a.id), &b)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "DELETE", &format!("/v1/blocks/{}", b.id), &a)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&app, "DELETE", &format!("/v1/blocks/{}", b.id), &a)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // Unblocking does not restore the friendship or visibility.
    assert_eq!(
        problem(&call(&app, "GET", &profile(a.id), &b).await),
        unknown
    );
    befriend(&app, &a, &b).await;

    // Unknown users and yourself cannot be blocked.
    assert_eq!(
        problem(&call(&app, "POST", &format!("/v1/blocks/{}", Uuid::now_v7()), &a).await),
        unknown
    );
    assert_eq!(
        call(&app, "POST", &format!("/v1/blocks/{}", a.id), &a)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn blocking_cancels_active_invites(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b) = (user(&pool).await, user(&pool).await);
    befriend(&app, &a, &b).await;
    let package: Uuid = sqlx::query_scalar(
        "INSERT INTO packages (slug, title, status, created_by) VALUES ('coop', 'Coop', 'published', $1) RETURNING id",
    )
    .bind(a.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    for (from, to, state) in [
        (a.id, b.id, "pending"),
        (b.id, a.id, "installing"),
        (a.id, b.id, "joined"),
    ] {
        sqlx::query(
            "INSERT INTO game_invites (from_user_id, to_user_id, package_id, state, expires_at) VALUES ($1, $2, $3, $4, now() + interval '1 hour')",
        )
        .bind(from)
        .bind(to)
        .bind(package)
        .bind(state)
        .execute(&pool)
        .await
        .unwrap();
    }
    call(&app, "POST", &format!("/v1/blocks/{}", a.id), &b).await;
    let states: Vec<String> =
        sqlx::query_scalar("SELECT state FROM game_invites ORDER BY created_at")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(states, ["cancelled", "cancelled", "joined"]);
}

#[sqlx::test(migrations = "./migrations")]
async fn profile_visibility(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b, c, d) = (
        user(&pool).await,
        user(&pool).await,
        user(&pool).await,
        user(&pool).await,
    );
    let get = |u: Uuid| format!("/v1/users/{u}");

    let (s, me) = call(&app, "GET", &get(a.id), &a).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(me["id"], a.id.to_string());
    assert!(me.get("discord_id").is_none() && me.get("role").is_none());

    assert_eq!(
        call(&app, "GET", &get(b.id), &a).await.0,
        StatusCode::NOT_FOUND
    );
    // Pending requests make both users visible.
    let code = new_code(&app, &b).await;
    redeem(&app, &a, &code).await;
    assert_eq!(call(&app, "GET", &get(b.id), &a).await.0, StatusCode::OK);
    assert_eq!(call(&app, "GET", &get(a.id), &b).await.0, StatusCode::OK);
    // A shared conversation does too, until one of them leaves.
    share_conversation(&pool, a.id, c.id).await;
    assert_eq!(call(&app, "GET", &get(c.id), &a).await.0, StatusCode::OK);
    sqlx::query("UPDATE conversation_members SET left_at = now() WHERE user_id = $1")
        .bind(c.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&app, "GET", &get(c.id), &a).await.0,
        StatusCode::NOT_FOUND
    );
    // Disabled accounts disappear.
    befriend(&app, &a, &d).await;
    sqlx::query("UPDATE users SET disabled_at = now() WHERE id = $1")
        .bind(d.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&app, "GET", &get(d.id), &a).await.0,
        StatusCode::NOT_FOUND
    );
    assert!(!ids(&list(&app, &a).await, "friends").contains(&d.id.to_string()));
}

#[sqlx::test(migrations = "./migrations")]
async fn pending_and_friend_limits(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b, c) = (user(&pool).await, user(&pool).await, user(&pool).await);

    // 100 pending outgoing requests from A.
    sqlx::query(
        "WITH u AS (
           INSERT INTO users (discord_id, username) SELECT (700000000000000000 + g)::text, 'filler' FROM generate_series(1, 100) g
           RETURNING id)
         INSERT INTO friendships (user_low, user_high, state, requested_by)
         SELECT least($1, id), greatest($1, id), 'pending', $1 FROM u",
    )
    .bind(a.id)
    .execute(&pool)
    .await
    .unwrap();
    let code = new_code(&app, &b).await;
    let (s, e) = redeem(&app, &a, &code).await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("pending_limit_reached"))
    );
    // The code was not used up by the refused request.
    assert_eq!(redeem(&app, &c, &code).await.0, StatusCode::CREATED);

    // 500 accepted friends for B.
    sqlx::query(
        "WITH u AS (
           INSERT INTO users (discord_id, username) SELECT (710000000000000000 + g)::text, 'filler' FROM generate_series(1, 500) g
           RETURNING id)
         INSERT INTO friendships (user_low, user_high, state, requested_by, accepted_at)
         SELECT least($1, id), greatest($1, id), 'accepted', $1, now() FROM u",
    )
    .bind(b.id)
    .execute(&pool)
    .await
    .unwrap();
    // B cannot accept C, and C's list keeps the pending request.
    let (s, e) = call(&app, "POST", &format!("/v1/friends/{}/accept", c.id), &b).await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("friend_limit_reached"))
    );
    assert_eq!(
        ids(&list(&app, &c).await, "outgoing"),
        vec![b.id.to_string()]
    );
    // Nor send new requests.
    let code = new_code(&app, &c).await;
    call(&app, "DELETE", &format!("/v1/friends/{}", b.id), &c).await;
    let (s, e) = redeem(&app, &b, &code).await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("friend_limit_reached"))
    );
    assert_eq!(
        list(&app, &b).await["friends"].as_array().unwrap().len(),
        500
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn friend_code_creation_and_requests_are_rate_limited(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b) = (user(&pool).await, user(&pool).await);
    for _ in 0..30 {
        new_code(&app, &a).await;
    }
    let (s, e) = call(&app, "POST", "/v1/friend-codes", &a).await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    // Requests have their own budget.
    for _ in 0..30 {
        assert_eq!(redeem(&app, &b, "ZZZZZZZZ").await.0, StatusCode::NOT_FOUND);
    }
    assert_eq!(
        redeem(&app, &b, "ZZZZZZZZ").await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn social_endpoints_need_a_session(pool: PgPool) {
    let app = common::app(pool.clone());
    for (method, uri) in [
        ("GET", "/v1/friends".to_string()),
        ("POST", "/v1/friend-codes".to_string()),
        ("GET", format!("/v1/users/{}", Uuid::now_v7())),
        ("POST", format!("/v1/blocks/{}", Uuid::now_v7())),
    ] {
        let resp = send(&app, bearer_request(method, &uri, "vga_nope")).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}
