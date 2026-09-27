//! A1-T04: Discord sign-in, tokens and sessions, with Discord mocked by wiremock.
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
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use common::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path},
};

const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const OWNER_ID: &str = "300000000000000001";

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

struct Env {
    app: Router,
    discord: MockServer,
    pool: PgPool,
}

async fn env(pool: PgPool) -> Env {
    let discord = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/oauth2/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("client_secret=test-secret"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "discord-at", "token_type": "Bearer"})),
        )
        .mount(&discord)
        .await;
    let config = config_with(&[
        ("VGAMES_DEV_FAKE_DISCORD", "false"),
        ("DISCORD_CLIENT_ID", "123456789012"),
        ("DISCORD_CLIENT_SECRET", "test-secret"),
        (
            "DISCORD_REDIRECT_URI",
            "http://localhost:8080/v1/auth/discord/callback",
        ),
        ("DISCORD_API_BASE", &discord.uri()),
        ("VGAMES_BOOTSTRAP_OWNER_DISCORD_ID", OWNER_ID),
    ]);
    let app = vgames_api::http::router(vgames_api::AppState::new(config, pool.clone()).unwrap());
    Env { app, discord, pool }
}

impl Env {
    /// The next Discord profile returned by `/api/users/@me`.
    async fn discord_user(&self, id: &str, username: &str) {
        self.discord.reset().await;
        Mock::given(method("POST"))
            .and(path("/api/oauth2/token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"access_token": "discord-at"})),
            )
            .mount(&self.discord)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/users/@me"))
            .and(header("authorization", "Bearer discord-at"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": id, "username": username, "global_name": "Global Name",
                "avatar": "a_0123456789abcdef0123456789abcdef"
            })))
            .mount(&self.discord)
            .await;
    }

    async fn set_mode(&self, mode: &str) {
        sqlx::query(
            "UPDATE server_settings SET value = to_jsonb($1::text) WHERE key = 'registration.mode'",
        )
        .bind(mode)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn start(&self, body: Value) -> (StatusCode, Value) {
        let resp = send(
            &self.app,
            json_request("POST", "/v1/auth/discord/start", &body),
        )
        .await;
        let status = resp.status();
        (status, body_json(resp).await)
    }

    /// start → Discord → callback; returns the callback response.
    async fn through_callback(&self, start_body: Value) -> axum::http::Response<Body> {
        let (status, body) = self.start(start_body).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let url = url::Url::parse(body["authorize_url"].as_str().unwrap()).unwrap();
        assert!(url.as_str().starts_with(&self.discord.uri()));
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .to_string();
        assert_eq!(
            url.query_pairs().find(|(k, _)| k == "scope").unwrap().1,
            "identify"
        );
        send(
            &self.app,
            get_req(&format!(
                "/v1/auth/discord/callback?code=discord-code&state={state}"
            )),
        )
        .await
    }

    /// Desktop sign-in up to the login code.
    async fn login_code(&self) -> String {
        let resp = self
            .through_callback(json!({
                "client": "desktop", "code_challenge": challenge(VERIFIER),
                "client_state": "launcher-state-0001", "device_name": "Test PC"
            }))
            .await;
        assert_eq!(resp.status(), StatusCode::FOUND);
        let location = resp.headers()["location"].to_str().unwrap().to_string();
        let url = url::Url::parse(&location).unwrap();
        assert_eq!(url.scheme(), "vgames");
        assert_eq!(
            url.query_pairs()
                .find(|(k, _)| k == "client_state")
                .unwrap()
                .1,
            "launcher-state-0001"
        );
        url.query_pairs()
            .find(|(k, _)| k == "code")
            .unwrap()
            .1
            .to_string()
    }

    async fn exchange(&self, body: Value) -> (StatusCode, Value) {
        let resp = send(&self.app, json_request("POST", "/v1/auth/token", &body)).await;
        let status = resp.status();
        (status, body_json(resp).await)
    }

    async fn desktop_login(&self) -> Value {
        let code = self.login_code().await;
        let (status, body) = self
            .exchange(json!({"grant_type": "authorization_code", "code": code, "code_verifier": VERIFIER}))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn bearer(&self, method: &str, uri: &str, token: &str) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = send(&self.app, req).await;
        let status = resp.status();
        let bytes = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn desktop_sign_in_with_pkce(pool: PgPool) {
    let e = env(pool).await;
    e.discord_user(OWNER_ID, "alice").await;
    let tokens = e.desktop_login().await;
    let access = tokens["access_token"].as_str().unwrap();
    assert!(
        access.starts_with("vga_")
            && tokens["refresh_token"]
                .as_str()
                .unwrap()
                .starts_with("vgr_")
    );
    assert_eq!(tokens["expires_in"], 900);
    assert_eq!(
        tokens["user"]["role"], "owner",
        "bootstrap account becomes owner"
    );
    assert_eq!(
        tokens["user"]["avatar_url"],
        format!(
            "https://cdn.discordapp.com/avatars/{OWNER_ID}/a_0123456789abcdef0123456789abcdef.gif"
        )
    );

    let (status, me) = e.bearer("GET", "/v1/me", access).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["user"]["username"], "alice");
    assert_eq!(me["session"]["kind"], "desktop");
    assert_eq!(me["session"]["device_name"], "Test PC");
    assert_eq!(me["session"]["current"], true);

    let (status, list) = e.bearer("GET", "/v1/me/sessions", access).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn pkce_mismatch_and_code_reuse_are_rejected(pool: PgPool) {
    let e = env(pool).await;
    e.discord_user(OWNER_ID, "alice").await;

    let code = e.login_code().await;
    let wrong = "A".repeat(43);
    let (status, body) = e
        .exchange(json!({"grant_type": "authorization_code", "code": code, "code_verifier": wrong}))
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("invalid_grant"))
    );
    // A failed attempt still burns the code.
    let (status, _) = e
        .exchange(
            json!({"grant_type": "authorization_code", "code": code, "code_verifier": VERIFIER}),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let code = e.login_code().await;
    let (status, _) = e
        .exchange(
            json!({"grant_type": "authorization_code", "code": code, "code_verifier": VERIFIER}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = e
        .exchange(
            json!({"grant_type": "authorization_code", "code": code, "code_verifier": VERIFIER}),
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("invalid_grant"))
    );
}

/// A refused sign-in: a 302 back to the client with `error=<code>`, no login code, no cookies.
fn assert_refused(resp: &axum::http::Response<Body>, code: &str) -> url::Url {
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert!(resp.headers().get("set-cookie").is_none());
    let location = url::Url::parse(resp.headers()["location"].to_str().unwrap()).unwrap();
    let error = location.query_pairs().find(|(k, _)| k == "error");
    assert_eq!(
        error.map(|(_, v)| v.into_owned()).as_deref(),
        Some(code),
        "{location}"
    );
    assert!(
        location.query_pairs().all(|(k, _)| k != "code"),
        "{location}"
    );
    location
}

#[sqlx::test(migrations = "./migrations")]
async fn oauth_state_is_single_use_and_denial_redirects(pool: PgPool) {
    let e = env(pool).await;
    e.discord_user(OWNER_ID, "alice").await;
    let (_, body) = e
        .start(json!({"client": "desktop", "code_challenge": challenge(VERIFIER)}))
        .await;
    let url = url::Url::parse(body["authorize_url"].as_str().unwrap()).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .to_string();

    let denied = send(
        &e.app,
        get_req(&format!(
            "/v1/auth/discord/callback?error=access_denied&state={state}"
        )),
    )
    .await;
    let location = assert_refused(&denied, "access_denied");
    assert_eq!(location.scheme(), "vgames");

    let again = send(
        &e.app,
        get_req(&format!("/v1/auth/discord/callback?code=x&state={state}")),
    )
    .await;
    assert_eq!(again.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(again).await["code"], "invalid_state");
}

#[sqlx::test(migrations = "./migrations")]
async fn start_validates_client_specific_fields(pool: PgPool) {
    let e = env(pool).await;
    let (status, body) = e.start(json!({"client": "desktop"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["errors"][0]["field"], "code_challenge");
    let (status, body) = e
        .start(json!({"client": "web", "return_to": "https://evil.example/"}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["errors"][0]["field"], "return_to");
    let (status, body) = e
        .start(json!({"client": "web", "return_to": "/admin/../etc"}))
        .await;
    assert_eq!(
        (status, body["errors"][0]["field"].as_str()),
        (StatusCode::BAD_REQUEST, Some("return_to"))
    );
    let (status, body) = e.start(json!({"client": "web", "surprise": 1})).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("unknown_field"))
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn refresh_rotates_and_reuse_revokes_the_session(pool: PgPool) {
    let e = env(pool).await;
    e.discord_user(OWNER_ID, "alice").await;
    let first = e.desktop_login().await;
    let (status, second) = e
        .exchange(json!({"grant_type": "refresh_token", "refresh_token": first["refresh_token"]}))
        .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_ne!(second["refresh_token"], first["refresh_token"]);

    // The old access token died with the rotation; the new one works.
    assert_eq!(
        e.bearer("GET", "/v1/me", first["access_token"].as_str().unwrap())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        e.bearer("GET", "/v1/me", second["access_token"].as_str().unwrap())
            .await
            .0,
        StatusCode::OK
    );

    // Presenting the rotated-out refresh token again means it leaked: the session ends.
    let (status, body) = e
        .exchange(json!({"grant_type": "refresh_token", "refresh_token": first["refresh_token"]}))
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("refresh_token_reused"))
    );
    assert_eq!(
        e.bearer("GET", "/v1/me", second["access_token"].as_str().unwrap())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, _) = e
        .exchange(json!({"grant_type": "refresh_token", "refresh_token": second["refresh_token"]}))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let reason: String = sqlx::query_scalar("SELECT revoked_reason FROM sessions")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(reason, "refresh_reuse");
}

#[sqlx::test(migrations = "./migrations")]
async fn registration_modes(pool: PgPool) {
    let e = env(pool).await;
    // Default mode is allowlist.
    e.discord_user("400000000000000001", "bob").await;
    let resp = e
        .through_callback(json!({"client": "desktop", "code_challenge": challenge(VERIFIER)}))
        .await;
    assert_refused(&resp, "not_allowlisted");

    sqlx::query("INSERT INTO registration_allowlist (discord_id) VALUES ('400000000000000001')")
        .execute(&e.pool)
        .await
        .unwrap();
    let bob = e.desktop_login().await;
    assert_eq!(bob["user"]["role"], "user");

    e.set_mode("closed").await;
    e.discord_user("400000000000000002", "carol").await;
    let resp = e
        .through_callback(json!({"client": "desktop", "code_challenge": challenge(VERIFIER)}))
        .await;
    assert_refused(&resp, "registration_closed");
    // Existing accounts keep signing in when registration closes.
    e.discord_user("400000000000000001", "bob").await;
    e.desktop_login().await;

    e.set_mode("open").await;
    e.discord_user("400000000000000002", "carol").await;
    e.desktop_login().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn web_sessions_use_cookies_with_csrf_and_origin_checks(pool: PgPool) {
    let e = env(pool).await;
    e.discord_user(OWNER_ID, "alice").await;
    let resp = e
        .through_callback(json!({"client": "web", "return_to": "/admin/packages"}))
        .await;
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(
        resp.headers()["location"],
        "http://localhost:8080/admin/packages"
    );
    let cookies: Vec<String> = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    let session = cookies
        .iter()
        .find(|c| c.starts_with("__Host-vgames_session="))
        .unwrap();
    let csrf = cookies
        .iter()
        .find(|c| c.starts_with("__Host-vgames_csrf="))
        .unwrap();
    for attr in ["Path=/", "Secure", "SameSite=Lax"] {
        assert!(
            session.contains(attr) && csrf.contains(attr),
            "{session} / {csrf}"
        );
    }
    assert!(session.contains("HttpOnly") && !csrf.contains("HttpOnly"));
    let session_val = session.split(';').next().unwrap().to_string();
    let csrf_val = csrf
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_string();

    let with_cookie = |method: &str, uri: &str| {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("cookie", format!("{session_val}; other=1"))
    };

    let me = send(
        &e.app,
        with_cookie("GET", "/v1/me").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(me.status(), StatusCode::OK);
    assert_eq!(body_json(me).await["session"]["kind"], "web");

    for req in [
        with_cookie("POST", "/v1/auth/logout")
            .header("origin", "http://localhost:8080")
            .body(Body::empty())
            .unwrap(),
        with_cookie("POST", "/v1/auth/logout")
            .header("x-csrf-token", &csrf_val)
            .body(Body::empty())
            .unwrap(),
        with_cookie("POST", "/v1/auth/logout")
            .header("x-csrf-token", &csrf_val)
            .header("origin", "https://evil.example")
            .body(Body::empty())
            .unwrap(),
        with_cookie("POST", "/v1/auth/logout")
            .header("x-csrf-token", "wrong")
            .header("origin", "http://localhost:8080")
            .body(Body::empty())
            .unwrap(),
    ] {
        let resp = send(&e.app, req).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(resp).await["code"], "csrf_failed");
    }

    let resp = send(
        &e.app,
        with_cookie("POST", "/v1/auth/logout")
            .header("x-csrf-token", &csrf_val)
            .header("origin", "http://localhost:8080")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(
        resp.headers()
            .get_all("set-cookie")
            .iter()
            .any(|v| v.to_str().unwrap().contains("Max-Age=0"))
    );
    let me = send(
        &e.app,
        with_cookie("GET", "/v1/me").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(me.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn a_web_cookie_is_not_a_bearer_token_and_vice_versa(pool: PgPool) {
    let e = env(pool).await;
    e.discord_user(OWNER_ID, "alice").await;
    let tokens = e.desktop_login().await;
    let access = tokens["access_token"].as_str().unwrap();
    let req = Request::builder()
        .uri("/v1/me")
        .header(
            "cookie",
            format!("__Host-vgames_session=vgs_{}", &access[4..]),
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&e.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn disabled_users_are_locked_out_and_their_sessions_revoked(pool: PgPool) {
    let e = env(pool).await;
    e.discord_user(OWNER_ID, "alice").await;
    let tokens = e.desktop_login().await;
    sqlx::query("UPDATE users SET disabled_at = now()")
        .execute(&e.pool)
        .await
        .unwrap();

    let (status, body) = e
        .bearer("GET", "/v1/me", tokens["access_token"].as_str().unwrap())
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("user_disabled"))
    );
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE revoked_at IS NULL")
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(active, 0);
    let (status, _) = e
        .exchange(json!({"grant_type": "refresh_token", "refresh_token": tokens["refresh_token"]}))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let resp = e
        .through_callback(json!({"client": "desktop", "code_challenge": challenge(VERIFIER)}))
        .await;
    assert_refused(&resp, "user_disabled");
}

#[sqlx::test(migrations = "./migrations")]
async fn sessions_can_be_listed_and_revoked(pool: PgPool) {
    let e = env(pool).await;
    e.discord_user(OWNER_ID, "alice").await;
    let a = e.desktop_login().await;
    let b = e.desktop_login().await;
    let (_, list) = e
        .bearer(
            "GET",
            "/v1/me/sessions",
            a["access_token"].as_str().unwrap(),
        )
        .await;
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let other = items.iter().find(|s| s["current"] == false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = e
        .bearer(
            "DELETE",
            &format!("/v1/me/sessions/{other}"),
            a["access_token"].as_str().unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        e.bearer("GET", "/v1/me", b["access_token"].as_str().unwrap())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, _) = e
        .bearer(
            "DELETE",
            &format!("/v1/me/sessions/{other}"),
            a["access_token"].as_str().unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = e
        .bearer(
            "DELETE",
            "/v1/me/sessions/not-a-uuid",
            a["access_token"].as_str().unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[sqlx::test(migrations = "./migrations")]
async fn no_plaintext_secret_is_stored(pool: PgPool) {
    let e = env(pool).await;
    e.discord_user(OWNER_ID, "alice").await;
    let mut secrets: Vec<String> = Vec::new();

    let code = e.login_code().await;
    secrets.push(code.clone());
    let (_, t) = e
        .exchange(
            json!({"grant_type": "authorization_code", "code": code, "code_verifier": VERIFIER}),
        )
        .await;
    secrets.push(t["access_token"].as_str().unwrap().to_string());
    secrets.push(t["refresh_token"].as_str().unwrap().to_string());
    let (_, t2) = e
        .exchange(json!({"grant_type": "refresh_token", "refresh_token": t["refresh_token"]}))
        .await;
    secrets.push(t2["access_token"].as_str().unwrap().to_string());
    secrets.push(t2["refresh_token"].as_str().unwrap().to_string());

    let (_, start) = e.start(json!({"client": "web"})).await;
    let url = url::Url::parse(start["authorize_url"].as_str().unwrap()).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .to_string();
    secrets.push(state.clone());
    let resp = send(
        &e.app,
        get_req(&format!(
            "/v1/auth/discord/callback?code=discord-code&state={state}"
        )),
    )
    .await;
    for c in resp.headers().get_all("set-cookie") {
        let v = c
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1
            .to_string();
        secrets.push(v);
    }
    assert_eq!(secrets.len(), 8);

    let mut dump = String::new();
    for table in [
        "sessions",
        "login_codes",
        "oauth_flows",
        "users",
        "audit_log",
    ] {
        let rows: Vec<String> = // Table names come from the constant list above.
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT row_to_json(t)::text FROM {table} t"))).fetch_all(&e.pool).await.unwrap();
        dump.push_str(&rows.join("\n"));
    }
    for s in &secrets {
        let raw_hex = hex::encode(s.as_bytes());
        assert!(
            !dump.contains(s.as_str()) && !dump.contains(&raw_hex),
            "plaintext secret stored: {s}"
        );
    }
    assert!(
        !dump.contains("discord-at"),
        "Discord access token must not be stored"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn fake_discord_signs_in_without_discord(pool: PgPool) {
    let app = app(pool);
    let body = json!({"client": "desktop", "code_challenge": challenge(VERIFIER)});
    let resp = send(&app, json_request("POST", "/v1/auth/discord/start", &body)).await;
    let url = url::Url::parse(body_json(resp).await["authorize_url"].as_str().unwrap()).unwrap();
    assert_eq!(url.path(), "/v1/auth/dev/fake-discord");
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .to_string();
    let page = send(
        &app,
        get_req(&format!("/v1/auth/dev/fake-discord?state={state}")),
    )
    .await;
    assert_eq!(page.status(), StatusCode::OK);
    let resp = send(
        &app,
        get_req(&format!(
            "/v1/auth/dev/fake-discord/submit?state={state}&id=500000000000000001&name=dev"
        )),
    )
    .await;
    let next = resp.headers()["location"]
        .to_str()
        .unwrap()
        .replace("http://localhost:8080", "");
    // Fake users still pass the registration policy (default allowlist).
    let resp = send(&app, get_req(&next)).await;
    assert_refused(&resp, "not_allowlisted");
}

/// A refused or failed sign-in goes back to the client that started it (01-security §4.1): the
/// launcher gets its `client_state` and a page with the reason, the admin UI its login page;
/// nothing is issued. Only an unknown `state` stays a 400 problem.
#[sqlx::test(migrations = "./migrations")]
async fn refused_sign_ins_return_to_the_client_that_started_them(pool: PgPool) {
    let e = env(pool).await;
    e.discord_user("400000000000000003", "dave").await;

    let resp = e
        .through_callback(json!({
            "client": "desktop", "code_challenge": challenge(VERIFIER),
            "client_state": "launcher-state-0002", "device_name": "Test PC"
        }))
        .await;
    let location = assert_refused(&resp, "not_allowlisted");
    assert_eq!(location.scheme(), "vgames");
    assert_eq!(
        location
            .query_pairs()
            .find(|(k, _)| k == "client_state")
            .map(|(_, v)| v.into_owned())
            .as_deref(),
        Some("launcher-state-0002")
    );
    assert_eq!(resp.headers()["cache-control"], "no-store");
    let page = String::from_utf8(
        http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(
        page.contains("only admits invited Discord accounts"),
        "{page}"
    );

    let resp = e
        .through_callback(json!({"client": "web", "return_to": "/admin/packages"}))
        .await;
    let location = assert_refused(&resp, "not_allowlisted");
    assert_eq!(
        location.as_str(),
        "http://localhost:8080/admin/login?error=not_allowlisted"
    );

    // Discord redirected without a code: the flow is spent and the client is told to retry.
    let (_, body) = e
        .start(json!({"client": "web", "return_to": "/admin/"}))
        .await;
    let url = url::Url::parse(body["authorize_url"].as_str().unwrap()).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .to_string();
    let resp = send(
        &e.app,
        get_req(&format!("/v1/auth/discord/callback?state={state}")),
    )
    .await;
    assert_refused(&resp, "sign_in_failed");
    let resp = send(
        &e.app,
        get_req(&format!("/v1/auth/discord/callback?code=x&state={state}")),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(resp).await["code"], "invalid_state");

    for table in ["users", "sessions", "login_codes"] {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&e.pool)
                .await
                .unwrap();
        assert_eq!(n, 0, "{table}");
    }
}
