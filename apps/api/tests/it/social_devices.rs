//! A4-T04: E2EE devices and the key directory (05-social §4.1, 05-social-notes §2.1).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use crate::common::{self, publishing::json_req, *};
use crate::social_presence::{assert_quiet, befriend, instance, next_event};

use std::collections::HashSet;

use axum::{Router, body::Body, http::Request, http::StatusCode};
use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;
use vgames_proto::social::canonical;

const SERVER_ID: &str = "01920000-0000-7000-8000-00000000abcd";
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD_NO_PAD;

pub(crate) fn server_id() -> Uuid {
    SERVER_ID.parse().unwrap()
}

pub(crate) fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).unwrap();
    b
}

/// A launcher's Olm account as far as the server can tell: an identity key and a signing key.
pub(crate) struct Account {
    pub(crate) signing: SigningKey,
    pub(crate) identity: String,
    next_key: u64,
}

impl Account {
    pub(crate) fn new() -> Self {
        Self {
            signing: SigningKey::from_bytes(&random32()),
            identity: B64.encode(random32()),
            next_key: 1,
        }
    }

    fn signing_key(&self) -> String {
        B64.encode(self.signing.verifying_key().as_bytes())
    }

    fn sign(&self, message: &str) -> String {
        B64.encode(self.signing.sign(message.as_bytes()).to_bytes())
    }

    pub(crate) fn register_body(&self, user: Uuid) -> Value {
        let message =
            canonical::device_keys(&self.identity, server_id(), &self.signing_key(), user);
        json!({
            "display_name": "Desk", "platform": "linux",
            "identity_key": self.identity, "signing_key": self.signing_key(),
            "keys_signature": self.sign(&message),
        })
    }

    pub(crate) fn otk(&mut self, fallback: bool) -> Value {
        let key_id = B64.encode(self.next_key.to_be_bytes());
        self.next_key += 1;
        let key = B64.encode(random32());
        let signature = self.sign(&canonical::one_time_key(fallback, &key, &key_id));
        json!({"key_id": key_id, "public_key": key, "signature": signature})
    }

    pub(crate) fn otks(&mut self, n: usize) -> Vec<Value> {
        (0..n).map(|_| self.otk(false)).collect()
    }
}

/// Another desktop session for an existing user.
pub(crate) async fn session_for(pool: &PgPool, user: Uuid) -> String {
    let token = format!(
        "vga_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random32())
    );
    sqlx::query(
        "INSERT INTO sessions (user_id, kind, access_token_hash, access_expires_at, refresh_token_hash, refresh_expires_at)
         VALUES ($1, 'desktop', $2, now() + interval '15 minutes', $3, now() + interval '30 days')",
    )
    .bind(user)
    .bind(Sha256::digest(token.as_bytes()).to_vec())
    .bind(Sha256::digest(random32()).to_vec())
    .execute(pool)
    .await
    .unwrap();
    token
}

pub(crate) struct Launcher {
    pub(crate) user: Uuid,
    pub(crate) token: String,
    pub(crate) account: Account,
    pub(crate) device: Uuid,
}

pub(crate) async fn post(
    app: &Router,
    uri: &str,
    token: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let resp = send(app, json_req("POST", uri, token, body)).await;
    let status = resp.status();
    (status, body_json(resp).await)
}

pub(crate) async fn get(app: &Router, uri: &str, token: &str) -> (StatusCode, Value) {
    let resp = send(app, bearer_request("GET", uri, token)).await;
    let status = resp.status();
    (status, body_json(resp).await)
}

/// A new user with a registered device.
pub(crate) async fn launcher(pool: &PgPool, app: &Router) -> Launcher {
    let (user, _, token) = seed_session(pool, "user").await;
    let account = Account::new();
    let (s, dev) = post(app, "/v1/devices", &token, &account.register_body(user)).await;
    assert_eq!(s, StatusCode::CREATED, "{dev}");
    Launcher {
        user,
        token,
        account,
        device: dev["id"].as_str().unwrap().parse().unwrap(),
    }
}

pub(crate) async fn upload(app: &Router, l: &Launcher, body: &Value) -> (StatusCode, Value) {
    post(
        app,
        &format!("/v1/devices/{}/one-time-keys", l.device),
        &l.token,
        body,
    )
    .await
}

pub(crate) async fn claim(app: &Router, l: &Launcher, devices: &[Uuid]) -> Vec<Value> {
    let (s, b) = post(
        app,
        "/v1/keys/claim",
        &l.token,
        &json!({"device_ids": devices}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    b["items"].as_array().unwrap().clone()
}

#[sqlx::test(migrations = "./migrations")]
async fn registration_binds_the_session_and_checks_the_self_signature(pool: PgPool) {
    let app = common::app(pool.clone());
    let (user, _, token) = seed_session(&pool, "user").await;
    let account = Account::new();

    // Signatures bound to another user or server, tampered, or by another key are refused.
    let mut wrong_user = account.register_body(user);
    wrong_user["keys_signature"] = json!(account.sign(&canonical::device_keys(
        &account.identity,
        server_id(),
        &account.signing_key(),
        Uuid::now_v7()
    )));
    let mut wrong_server = account.register_body(user);
    wrong_server["keys_signature"] = json!(account.sign(&canonical::device_keys(
        &account.identity,
        Uuid::now_v7(),
        &account.signing_key(),
        user
    )));
    let mut other_key = account.register_body(user);
    other_key["keys_signature"] = json!(Account::new().sign(&canonical::device_keys(
        &account.identity,
        server_id(),
        &account.signing_key(),
        user
    )));
    let mut swapped_identity = account.register_body(user);
    swapped_identity["identity_key"] = json!(B64.encode(random32()));
    for bad in [wrong_user, wrong_server, other_key, swapped_identity] {
        let (s, e) = post(&app, "/v1/devices", &token, &bad).await;
        assert_eq!(
            (s, e["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("bad_signature")),
            "{e}"
        );
    }
    // Malformed keys are validation errors.
    for (field, value) in [
        ("identity_key", json!("short")),
        (
            "signing_key",
            json!(format!("{}=", &account.signing_key()[..42])),
        ),
        ("keys_signature", json!("A".repeat(85))),
        ("display_name", json!("  ")),
        ("display_name", json!("x".repeat(65))),
    ] {
        let mut body = account.register_body(user);
        body[field] = value;
        let (s, e) = post(&app, "/v1/devices", &token, &body).await;
        assert_eq!(
            (s, e["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("validation_failed")),
            "{field}: {e}"
        );
    }
    assert!(
        get(&app, "/v1/devices", &token).await.1["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let (s, dev) = post(&app, "/v1/devices", &token, &account.register_body(user)).await;
    assert_eq!(s, StatusCode::CREATED, "{dev}");
    assert_eq!(
        (
            dev["current"].as_bool(),
            dev["one_time_keys_available"].as_i64(),
            dev["has_fallback_key"].as_bool()
        ),
        (Some(true), Some(0), Some(false))
    );
    assert_eq!(dev["identity_key"], account.identity);
    let id = dev["id"].as_str().unwrap().to_string();
    assert_eq!(get(&app, "/v1/me", &token).await.1["device_id"], id);

    // Retrying is idempotent; other keys for this session are a conflict.
    let (s, again) = post(&app, "/v1/devices", &token, &account.register_body(user)).await;
    assert_eq!(
        (s, again["id"].as_str()),
        (StatusCode::CREATED, Some(id.as_str()))
    );
    let (s, e) = post(
        &app,
        "/v1/devices",
        &token,
        &Account::new().register_body(user),
    )
    .await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("device_already_registered"))
    );

    // Signing in again with the same account binds the new session to the same device.
    let token2 = session_for(&pool, user).await;
    let (s, rebound) = post(&app, "/v1/devices", &token2, &account.register_body(user)).await;
    assert_eq!(
        (s, rebound["id"].as_str()),
        (StatusCode::CREATED, Some(id.as_str()))
    );
    // A new account on a third session is a second device.
    let token3 = session_for(&pool, user).await;
    let (s, second) = post(
        &app,
        "/v1/devices",
        &token3,
        &Account::new().register_body(user),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let list = get(&app, "/v1/devices", &token3).await.1;
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items.iter().filter(|d| d["current"] == true).count(), 1);
    assert_eq!(items[1]["id"], second["id"]);

    // Another user cannot take these keys, even with a valid signature bound to them.
    let (other, _, other_token) = seed_session(&pool, "user").await;
    let (s, e) = post(
        &app,
        "/v1/devices",
        &other_token,
        &account.register_body(other),
    )
    .await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("device_keys_in_use"))
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn web_sessions_cannot_register_devices(pool: PgPool) {
    let app = common::app(pool.clone());
    let (user, _, _) = seed_session(&pool, "user").await;
    let token = format!(
        "vgs_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random32())
    );
    let csrf = "csrf-token-for-test";
    sqlx::query(
        "INSERT INTO sessions (user_id, kind, access_token_hash, access_expires_at, csrf_token_hash)
         VALUES ($1, 'web', $2, now() + interval '15 minutes', $3)",
    )
    .bind(user)
    .bind(Sha256::digest(token.as_bytes()).to_vec())
    .bind(Sha256::digest(csrf.as_bytes()).to_vec())
    .execute(&pool)
    .await
    .unwrap();
    let body = Account::new().register_body(user);
    let req = Request::builder()
        .method("POST")
        .uri("/v1/devices")
        .header("cookie", format!("__Host-vgames_session={token}"))
        .header("x-csrf-token", csrf)
        .header("origin", "http://localhost:8080")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = send(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(resp).await["code"], "desktop_session_required");
}

#[sqlx::test(migrations = "./migrations")]
async fn one_time_key_uploads_check_every_signature_and_the_cap(pool: PgPool) {
    let app = common::app(pool.clone());
    let mut a = launcher(&pool, &app).await;

    let mut keys = a.account.otks(50);
    // One bad signature refuses the whole upload.
    keys[3]["signature"] = json!(a.account.sign("something else"));
    let fallback = a.account.otk(true);
    let (s, e) = upload(
        &app,
        &a,
        &json!({"one_time_keys": keys, "fallback_key": fallback}),
    )
    .await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_signature"))
    );
    assert_eq!(e["errors"][0]["field"], "one_time_keys[3].signature");
    // A one-time key cannot be uploaded as the fallback key, nor the reverse (the flag is signed).
    let relabelled = a.account.otk(false);
    let (s, e) = upload(&app, &a, &json!({"fallback_key": relabelled})).await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_signature"))
    );
    let as_otk = a.account.otk(true);
    let (s, _) = upload(&app, &a, &json!({"one_time_keys": [as_otk]})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let devices = get(&app, "/v1/devices", &a.token).await.1;
    assert_eq!(devices["items"][0]["one_time_keys_available"], 0);

    let keys = a.account.otks(50);
    let (s, stored) = upload(
        &app,
        &a,
        &json!({"one_time_keys": keys, "fallback_key": fallback}),
    )
    .await;
    assert_eq!(
        (s, stored["available"].as_i64()),
        (StatusCode::OK, Some(50)),
        "{stored}"
    );
    // Re-uploading the same keys changes nothing; a reused key id with another key is refused.
    let (s, stored) = upload(&app, &a, &json!({"one_time_keys": keys})).await;
    assert_eq!(
        (s, stored["available"].as_i64()),
        (StatusCode::OK, Some(50))
    );
    let mut reused = a.account.otk(false);
    reused["key_id"] = keys[0]["key_id"].clone();
    reused["signature"] = json!(a.account.sign(&canonical::one_time_key(
        false,
        reused["public_key"].as_str().unwrap(),
        reused["key_id"].as_str().unwrap()
    )));
    let (s, e) = upload(&app, &a, &json!({"one_time_keys": [reused]})).await;
    assert_eq!(
        (s, e["errors"][0]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("key_id_reused"))
    );

    // At most 100 unclaimed keys.
    let batch = json!({"one_time_keys": a.account.otks(51)});
    let (s, e) = upload(&app, &a, &batch).await;
    assert_eq!(
        (s, e["errors"][0]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("too_many_keys"))
    );
    let batch = json!({"one_time_keys": a.account.otks(50)});
    let (s, stored) = upload(&app, &a, &batch).await;
    assert_eq!(
        (s, stored["available"].as_i64()),
        (StatusCode::OK, Some(100))
    );
    let batch = json!({"one_time_keys": a.account.otks(101)});
    let (s, _) = upload(&app, &a, &batch).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let dup = a.account.otk(false);
    let (s, _) = upload(&app, &a, &json!({"one_time_keys": [dup.clone(), dup]})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let list = get(&app, "/v1/devices", &a.token).await.1;
    assert_eq!(
        (
            list["items"][0]["one_time_keys_available"].as_i64(),
            list["items"][0]["has_fallback_key"].as_bool()
        ),
        (Some(100), Some(true))
    );

    // Only the device itself uploads: not another device of the same user, not another user.
    let token2 = session_for(&pool, a.user).await;
    let (s, _) = post(
        &app,
        "/v1/devices",
        &token2,
        &Account::new().register_body(a.user),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let b = launcher(&pool, &app).await;
    for token in [&token2, &b.token] {
        let (s, _) = post(
            &app,
            &format!("/v1/devices/{}/one-time-keys", a.device),
            token,
            &json!({"one_time_keys": a.account.otks(1)}),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn claims_follow_relationships_and_fall_back_when_exhausted(pool: PgPool) {
    let app = common::app(pool.clone());
    let a = launcher(&pool, &app).await;
    let mut friend = launcher(&pool, &app).await;
    let mut stranger = launcher(&pool, &app).await;
    let mut member = launcher(&pool, &app).await;
    befriend(&pool, a.user, friend.user, "accepted").await;
    let conv: Uuid = sqlx::query_scalar(
        "INSERT INTO conversations (kind, created_by) VALUES ('party', $1) RETURNING id",
    )
    .bind(a.user)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO conversation_members (conversation_id, user_id) VALUES ($1, $2), ($1, $3)",
    )
    .bind(conv)
    .bind(a.user)
    .bind(member.user)
    .execute(&pool)
    .await
    .unwrap();
    // A's second device.
    let token2 = session_for(&pool, a.user).await;
    let mut own = Account::new();
    let (_, own_dev) = post(&app, "/v1/devices", &token2, &own.register_body(a.user)).await;
    let own_id: Uuid = own_dev["id"].as_str().unwrap().parse().unwrap();
    post(
        &app,
        &format!("/v1/devices/{own_id}/one-time-keys"),
        &token2,
        &json!({"one_time_keys": own.otks(1)}),
    )
    .await;

    for l in [&mut friend, &mut stranger, &mut member] {
        let keys = l.account.otks(2);
        let fallback = l.account.otk(true);
        let (s, _) = upload(
            &app,
            l,
            &json!({"one_time_keys": keys, "fallback_key": fallback}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
    }

    let got = claim(
        &app,
        &a,
        &[
            friend.device,
            stranger.device,
            member.device,
            own_id,
            a.device,
            Uuid::now_v7(),
        ],
    )
    .await;
    let devices: HashSet<String> = got
        .iter()
        .map(|k| k["device_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        devices,
        HashSet::from([
            friend.device.to_string(),
            member.device.to_string(),
            own_id.to_string()
        ])
    );
    // Claimed keys verify against the device's signing key, flag included.
    let fk = got
        .iter()
        .find(|k| k["device_id"] == friend.device.to_string())
        .unwrap();
    assert_eq!(fk["is_fallback"], false);
    let message = canonical::one_time_key(
        false,
        fk["public_key"].as_str().unwrap(),
        fk["key_id"].as_str().unwrap(),
    );
    let sig = ed25519_dalek::Signature::from_bytes(
        &B64.decode(fk["signature"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
    );
    friend
        .account
        .signing
        .verifying_key()
        .verify_strict(message.as_bytes(), &sig)
        .unwrap();

    // Two one-time keys, then the fallback key every time.
    let second = claim(&app, &a, &[friend.device]).await;
    assert_eq!(second[0]["is_fallback"], false);
    assert_ne!(second[0]["key_id"], fk["key_id"]);
    for _ in 0..2 {
        let k = claim(&app, &a, &[friend.device]).await;
        assert_eq!(k[0]["is_fallback"], true);
    }
    let devices = get(&app, "/v1/devices", &friend.token).await.1;
    assert_eq!(devices["items"][0]["one_time_keys_available"], 0);

    // Blocks stop claims; so does leaving the conversation.
    sqlx::query("INSERT INTO user_blocks (blocker_id, blocked_id) VALUES ($1, $2)")
        .bind(friend.user)
        .bind(a.user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE conversation_members SET left_at = now() WHERE user_id = $1")
        .bind(member.user)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        claim(&app, &a, &[friend.device, member.device])
            .await
            .is_empty()
    );

    // Validation and a session without a device.
    let (s, _) = post(&app, "/v1/keys/claim", &a.token, &json!({"device_ids": []})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = post(
        &app,
        "/v1/keys/claim",
        &a.token,
        &json!({"device_ids": [own_id, own_id]}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let many: Vec<Uuid> = (0..65).map(|_| Uuid::now_v7()).collect();
    let (s, _) = post(
        &app,
        "/v1/keys/claim",
        &a.token,
        &json!({"device_ids": many}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, _, no_device) = seed_session(&pool, "user").await;
    let (s, e) = post(
        &app,
        "/v1/keys/claim",
        &no_device,
        &json!({"device_ids": [friend.device]}),
    )
    .await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("device_required"))
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn parallel_claims_never_hand_out_a_key_twice(pool: PgPool) {
    let app = common::app(pool.clone());
    let mut target = launcher(&pool, &app).await;
    let keys = target.account.otks(100);
    let fallback = target.account.otk(true);
    let (s, _) = upload(
        &app,
        &target,
        &json!({"one_time_keys": keys, "fallback_key": fallback}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    // Ten friends with their own devices claim ten keys each, all at once.
    let mut claimers = Vec::new();
    for _ in 0..10 {
        let c = launcher(&pool, &app).await;
        befriend(&pool, c.user, target.user, "accepted").await;
        claimers.push(c);
    }
    let device = target.device;
    let mut tasks = Vec::new();
    for c in &claimers {
        for _ in 0..10 {
            let app = app.clone();
            let token = c.token.clone();
            tasks.push(tokio::spawn(async move {
                let (s, b) = post(
                    &app,
                    "/v1/keys/claim",
                    &token,
                    &json!({"device_ids": [device]}),
                )
                .await;
                assert_eq!(s, StatusCode::OK, "{b}");
                b["items"][0].clone()
            }));
        }
    }
    let mut ids = HashSet::new();
    for t in tasks {
        let k = t.await.unwrap();
        assert_eq!(
            k["is_fallback"], false,
            "a claim fell back while keys remained: {k}"
        );
        assert!(
            ids.insert(k["key_id"].as_str().unwrap().to_string()),
            "key handed out twice"
        );
    }
    assert_eq!(ids.len(), 100);
    let unclaimed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM device_one_time_keys WHERE device_id = $1 AND NOT is_fallback AND claimed_at IS NULL",
    )
    .bind(device)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(unclaimed, 0);
    let next = claim(&app, &claimers[0], &[device]).await;
    assert_eq!(next[0]["is_fallback"], true);

    // social.sweep forgets claimed one-time keys after 30 days, never the fallback key.
    sqlx::query("UPDATE device_one_time_keys SET claimed_at = now() - interval '31 days' WHERE device_id = $1")
        .bind(device)
        .execute(&pool)
        .await
        .unwrap();
    vgames_api::social::sweep_once(&common::state(pool.clone()))
        .await
        .unwrap();
    let left: Vec<bool> =
        sqlx::query_scalar("SELECT is_fallback FROM device_one_time_keys WHERE device_id = $1")
            .bind(device)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(left, vec![true]);
}

#[sqlx::test(migrations = "./migrations")]
async fn revoking_a_device_ends_its_session_and_tells_contacts(pool: PgPool) {
    let inst = instance(&pool).await;
    let app = inst.app();
    let (a, _, token_a) = seed_session(&pool, "user").await;
    let (b, _, token_b) = seed_session(&pool, "user").await;
    let (stranger, _, token_s) = seed_session(&pool, "user").await;
    befriend(&pool, a, b, "accepted").await;
    let mut wb = inst.connect(&token_b).await;
    let mut ws = inst.connect(&token_s).await;

    let first = Account::new();
    let (s, d1) = post(&app, "/v1/devices", &token_a, &first.register_body(a)).await;
    assert_eq!(s, StatusCode::CREATED);
    let d1: Uuid = d1["id"].as_str().unwrap().parse().unwrap();
    let ev = next_event(&mut wb).await;
    assert_eq!(
        (ev["type"].as_str(), ev["data"].clone()),
        (Some("device.added"), json!({"user_id": a, "device_id": d1}))
    );
    assert_quiet(&inst.state, &mut ws, stranger).await;

    let token_a2 = session_for(&pool, a).await;
    let mut second = Account::new();
    let (_, d2) = post(&app, "/v1/devices", &token_a2, &second.register_body(a)).await;
    let d2: Uuid = d2["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        next_event(&mut wb).await["data"]["device_id"],
        d2.to_string()
    );
    post(
        &app,
        &format!("/v1/devices/{d2}/one-time-keys"),
        &token_a2,
        &json!({"one_time_keys": second.otks(3)}),
    )
    .await;

    // Friends see both devices' keys; strangers see nothing.
    let (s, keys) = get(&app, &format!("/v1/users/{a}/devices"), &token_b).await;
    assert_eq!(s, StatusCode::OK);
    let items = keys["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let k = &items[0];
    let message = canonical::device_keys(
        k["identity_key"].as_str().unwrap(),
        server_id(),
        k["signing_key"].as_str().unwrap(),
        a,
    );
    let sig = ed25519_dalek::Signature::from_bytes(
        &B64.decode(k["keys_signature"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
    );
    first
        .signing
        .verifying_key()
        .verify_strict(message.as_bytes(), &sig)
        .unwrap();
    assert_eq!(
        get(&app, &format!("/v1/users/{a}/devices"), &token_s)
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    // Only the owner can revoke, and only once.
    let resp = send(
        &app,
        bearer_request("DELETE", &format!("/v1/devices/{d2}"), &token_b),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = send(
        &app,
        bearer_request("DELETE", &format!("/v1/devices/{d2}"), &token_a),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = send(
        &app,
        bearer_request("DELETE", &format!("/v1/devices/{d2}"), &token_a),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let ev = next_event(&mut wb).await;
    assert_eq!(
        (ev["type"].as_str(), ev["data"].clone()),
        (
            Some("device.revoked"),
            json!({"user_id": a, "device_id": d2})
        )
    );
    assert_quiet(&inst.state, &mut ws, stranger).await;
    // The session bound to it is over; the other device keeps working.
    assert_eq!(
        get(&app, "/v1/me", &token_a2).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (_, mine) = get(&app, "/v1/devices", &token_a).await;
    assert_eq!(mine["items"].as_array().unwrap().len(), 1);
    // Its keys are gone and it is neither listed nor claimable.
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM device_one_time_keys WHERE device_id = $1")
            .bind(d2)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(left, 0);
    let (_, keys) = get(&app, &format!("/v1/users/{a}/devices"), &token_b).await;
    assert_eq!(keys["items"].as_array().unwrap().len(), 1);
    // Its keys can never be registered again.
    let token_a3 = session_for(&pool, a).await;
    let (s, e) = post(&app, "/v1/devices", &token_a3, &second.register_body(a)).await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("device_keys_in_use"))
    );
}
