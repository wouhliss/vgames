//! A4-T05: conversations and the ciphertext relay (05-social §4.1, §4.4).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use crate::common::{self, *};
use crate::social_devices::{Account, Launcher, get, launcher, post, server_id, session_for};
use crate::social_presence::{assert_quiet, befriend, instance, next_event};

use std::collections::HashSet;

use axum::{Router, http::StatusCode};
use base64::Engine as _;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use vgames_proto::social::canonical;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn envelope(device: Uuid, bytes: &[u8]) -> Value {
    json!({"recipient_device_id": device, "olm_message_type": 1, "ciphertext": B64.encode(bytes)})
}

async fn direct(app: &Router, from: &Launcher, to: Uuid) -> (StatusCode, Value) {
    post(
        app,
        "/v1/conversations",
        &from.token,
        &json!({"kind": "direct", "user_id": to}),
    )
    .await
}

async fn send(
    app: &Router,
    from: &Launcher,
    conv: &str,
    cmid: Uuid,
    envelopes: Vec<Value>,
) -> (StatusCode, Value) {
    let resp = common::send(
        app,
        crate::common::publishing::json_req(
            "POST",
            &format!("/v1/conversations/{conv}/messages"),
            &from.token,
            &json!({"client_message_id": cmid, "envelopes": envelopes}),
        ),
    )
    .await;
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn inbox(app: &Router, token: &str) -> Vec<Value> {
    let (s, b) = get(app, "/v1/inbox?limit=200", token).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    b["items"].as_array().unwrap().clone()
}

async fn add_device(pool: &PgPool, app: &Router, user: Uuid) -> (String, Uuid) {
    let token = session_for(pool, user).await;
    let (s, d) = post(
        app,
        "/v1/devices",
        &token,
        &Account::new().register_body(user),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    (token, d["id"].as_str().unwrap().parse().unwrap())
}

#[sqlx::test(migrations = "./migrations")]
async fn conversations_need_friendship(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b, c, d) = (
        launcher(&pool, &app).await,
        launcher(&pool, &app).await,
        launcher(&pool, &app).await,
        launcher(&pool, &app).await,
    );
    befriend(&pool, a.user, b.user, "accepted").await;
    befriend(&pool, a.user, c.user, "accepted").await;
    befriend(&pool, a.user, d.user, "pending").await;

    // Direct: get or create, the same conversation from either side.
    let (s, conv) = direct(&app, &a, b.user).await;
    assert_eq!(s, StatusCode::CREATED, "{conv}");
    assert_eq!(conv["kind"], "direct");
    assert_eq!(conv["members"].as_array().unwrap().len(), 2);
    let (s, again) = direct(&app, &b, a.user).await;
    assert_eq!((s, &again["id"]), (StatusCode::OK, &conv["id"]));

    // Not friends (pending, stranger, unknown), yourself, or blocked.
    for to in [d.user, Uuid::now_v7()] {
        let (s, e) = direct(&app, &a, to).await;
        assert_eq!(
            (s, e["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("not_allowed"))
        );
    }
    assert_eq!(direct(&app, &a, a.user).await.0, StatusCode::BAD_REQUEST);
    sqlx::query("INSERT INTO user_blocks (blocker_id, blocked_id) VALUES ($1, $2)")
        .bind(c.user)
        .bind(a.user)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(direct(&app, &a, c.user).await.0, StatusCode::FORBIDDEN);

    // Parties: 1-15 friends of the creator.
    let (s, party) = post(
        &app,
        "/v1/conversations",
        &a.token,
        &json!({"kind": "party", "user_ids": [b.user]}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{party}");
    assert_eq!(party["members"].as_array().unwrap().len(), 2);
    for bad in [
        json!({"kind": "party", "user_ids": []}),
        json!({"kind": "party", "user_ids": [b.user, b.user]}),
        json!({"kind": "party", "user_ids": [a.user, b.user]}),
        json!({"kind": "party", "user_ids": (0..16).map(|_| Uuid::now_v7()).collect::<Vec<_>>()}),
        json!({"kind": "group", "user_ids": [b.user]}),
    ] {
        assert_eq!(
            post(&app, "/v1/conversations", &a.token, &bad).await.0,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    for bad in [json!([b.user, d.user]), json!([b.user, c.user])] {
        let (s, _) = post(
            &app,
            "/v1/conversations",
            &a.token,
            &json!({"kind": "party", "user_ids": bad}),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }

    // Listing: newest activity first, paginated; strangers see none of it.
    send(
        &app,
        &a,
        conv["id"].as_str().unwrap(),
        Uuid::now_v7(),
        vec![envelope(b.device, b"x")],
    )
    .await;
    let (_, page) = get(&app, "/v1/conversations?limit=1", &a.token).await;
    assert_eq!(page["items"][0]["id"], conv["id"]);
    assert!(page["items"][0]["last_activity_at"].is_string());
    let cursor = page["next_cursor"].as_str().unwrap();
    let (_, page2) = get(
        &app,
        &format!("/v1/conversations?limit=1&cursor={cursor}"),
        &a.token,
    )
    .await;
    assert_eq!(page2["items"][0]["id"], party["id"]);
    assert!(page2.get("next_cursor").is_none());
    let (s, _) = get(
        &app,
        &format!("/v1/conversations?limit=1&cursor={cursor}"),
        &b.token,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "a cursor belongs to its user");
    let (_, none) = get(&app, "/v1/conversations", &d.token).await;
    assert!(none["items"].as_array().unwrap().is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn sending_checks_membership_devices_and_reports_unknown_devices(pool: PgPool) {
    let inst = instance(&pool).await;
    let app = inst.app();
    let (a, b, c) = (
        launcher(&pool, &app).await,
        launcher(&pool, &app).await,
        launcher(&pool, &app).await,
    );
    befriend(&pool, a.user, b.user, "accepted").await;
    let (_, conv) = direct(&app, &a, b.user).await;
    let conv = conv["id"].as_str().unwrap().to_string();
    let (_, b_second) = add_device(&pool, &app, b.user).await;
    let (_, a_other) = add_device(&pool, &app, a.user).await;
    let mut wb = inst.connect(&b.token).await;

    // Everything addressed: accepted, no unknown devices, one inbox.new for B.
    let cmid = Uuid::now_v7();
    let all = vec![
        envelope(b.device, b"1"),
        envelope(b_second, b"2"),
        envelope(a_other, b"3"),
    ];
    let (s, r) = send(&app, &a, &conv, cmid, all.clone()).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{r}");
    assert_eq!(r["accepted"], 3);
    assert!(r.get("unknown_devices").is_none(), "{r}");
    let ev = next_event(&mut wb).await;
    assert_eq!(
        (ev["type"].as_str(), ev["data"].clone()),
        (
            Some("inbox.new"),
            json!({"conversation_id": conv, "count": 2})
        )
    );

    // Retrying is idempotent: nothing stored twice, no second nudge.
    let (s, r) = send(&app, &a, &conv, cmid, all).await;
    assert_eq!((s, r["accepted"].as_i64()), (StatusCode::ACCEPTED, Some(3)));
    assert_quiet(&inst.state, &mut wb, b.user).await;
    assert_eq!(inbox(&app, &b.token).await.len(), 1);

    // A partial send names what it left out, including the sender's own other device.
    let (_, r) = send(
        &app,
        &a,
        &conv,
        Uuid::now_v7(),
        vec![envelope(b.device, b"x")],
    )
    .await;
    let unknown: HashSet<String> = r["unknown_devices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        unknown,
        HashSet::from([b_second.to_string(), a_other.to_string()])
    );
    // A retry covering the rest completes it.
    let (_, r) = send(
        &app,
        &a,
        &conv,
        Uuid::now_v7(),
        vec![
            envelope(b.device, b"y"),
            envelope(b_second, b"y"),
            envelope(a_other, b"y"),
        ],
    )
    .await;
    assert!(r.get("unknown_devices").is_none());

    // Not a device of this conversation: a stranger's, a revoked one, the sender's own, unknown.
    let revoked = add_device(&pool, &app, b.user).await.1;
    let resp = common::send(
        &app,
        bearer_request("DELETE", &format!("/v1/devices/{revoked}"), &b.token),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    for device in [c.device, revoked, a.device, Uuid::now_v7()] {
        let (s, e) = send(
            &app,
            &a,
            &conv,
            Uuid::now_v7(),
            vec![envelope(device, b"z")],
        )
        .await;
        assert_eq!(
            (s, e["errors"][0]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("unknown_recipient")),
            "{device}"
        );
    }
    // Non-members, unknown conversations, and sessions without a device.
    let (s, _) = send(
        &app,
        &c,
        &conv,
        Uuid::now_v7(),
        vec![envelope(b.device, b"z")],
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = send(
        &app,
        &a,
        &Uuid::now_v7().to_string(),
        Uuid::now_v7(),
        vec![envelope(b.device, b"z")],
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, _, bare) = seed_session(&pool, "user").await;
    let bare = Launcher {
        user: Uuid::nil(),
        token: bare,
        account: Account::new(),
        device: Uuid::nil(),
    };
    let (s, e) = send(
        &app,
        &bare,
        &conv,
        Uuid::now_v7(),
        vec![envelope(b.device, b"z")],
    )
    .await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("device_required"))
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn size_and_shape_limits(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b) = (launcher(&pool, &app).await, launcher(&pool, &app).await);
    befriend(&pool, a.user, b.user, "accepted").await;
    let (_, conv) = direct(&app, &a, b.user).await;
    let conv = conv["id"].as_str().unwrap().to_string();

    let max = vec![7u8; 64 * 1024];
    let (s, _) = send(
        &app,
        &a,
        &conv,
        Uuid::now_v7(),
        vec![envelope(b.device, &max)],
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let over = vec![7u8; 64 * 1024 + 1];
    let (s, e) = send(
        &app,
        &a,
        &conv,
        Uuid::now_v7(),
        vec![envelope(b.device, &over)],
    )
    .await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("payload_too_large"))
    );

    let mut many = Vec::new();
    for _ in 0..65 {
        many.push(envelope(Uuid::now_v7(), b"x"));
    }
    assert_eq!(
        send(&app, &a, &conv, Uuid::now_v7(), many).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send(&app, &a, &conv, Uuid::now_v7(), vec![]).await.0,
        StatusCode::BAD_REQUEST
    );
    for bad in [
        json!({"recipient_device_id": b.device, "olm_message_type": 2, "ciphertext": "AAAA"}),
        json!({"recipient_device_id": b.device, "olm_message_type": 0, "ciphertext": "not base64!"}),
        json!({"recipient_device_id": b.device, "olm_message_type": 0, "ciphertext": ""}),
        json!({"recipient_device_id": b.device, "olm_message_type": 0, "ciphertext": "AAAA", "extra": 1}),
    ] {
        assert_eq!(
            send(&app, &a, &conv, Uuid::now_v7(), vec![bad.clone()])
                .await
                .0,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    let dup = vec![envelope(b.device, b"x"), envelope(b.device, b"y")];
    assert_eq!(
        send(&app, &a, &conv, Uuid::now_v7(), dup).await.0,
        StatusCode::BAD_REQUEST
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn blocks_close_direct_conversations_and_hide_party_members(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b, c) = (
        launcher(&pool, &app).await,
        launcher(&pool, &app).await,
        launcher(&pool, &app).await,
    );
    befriend(&pool, a.user, b.user, "accepted").await;
    befriend(&pool, a.user, c.user, "accepted").await;
    let (_, dm) = direct(&app, &a, b.user).await;
    let dm = dm["id"].as_str().unwrap().to_string();
    let (_, party) = post(
        &app,
        "/v1/conversations",
        &a.token,
        &json!({"kind": "party", "user_ids": [b.user, c.user]}),
    )
    .await;
    let party = party["id"].as_str().unwrap().to_string();

    let resp = common::send(
        &app,
        bearer_request("POST", &format!("/v1/blocks/{}", a.user), &b.token),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    // The direct conversation answers 404 in both directions.
    assert_eq!(
        send(
            &app,
            &a,
            &dm,
            Uuid::now_v7(),
            vec![envelope(b.device, b"x")]
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (a2_token, a2_device) = add_device(&pool, &app, a.user).await;
    let a2 = Launcher {
        user: a.user,
        token: a2_token,
        account: Account::new(),
        device: a2_device,
    };
    assert_eq!(
        send(
            &app,
            &a2,
            &dm,
            Uuid::now_v7(),
            vec![envelope(a.device, b"x")]
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(
            &app,
            &b,
            &dm,
            Uuid::now_v7(),
            vec![envelope(a.device, b"x")]
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    // In the party, B is not addressable by A and not reported as unknown.
    let (s, e) = send(
        &app,
        &a,
        &party,
        Uuid::now_v7(),
        vec![envelope(b.device, b"x")],
    )
    .await;
    assert_eq!(
        (s, e["errors"][0]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("unknown_recipient"))
    );
    let (s, r) = send(
        &app,
        &a,
        &party,
        Uuid::now_v7(),
        vec![envelope(c.device, b"x"), envelope(a2.device, b"x")],
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert!(r.get("unknown_devices").is_none(), "{r}");
    // C still reaches both.
    let (_, r) = send(
        &app,
        &c,
        &party,
        Uuid::now_v7(),
        vec![envelope(a.device, b"x")],
    )
    .await;
    let unknown: HashSet<String> = r["unknown_devices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        unknown,
        HashSet::from([b.device.to_string(), a2.device.to_string()])
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn each_device_reads_and_acks_only_its_own_inbox(pool: PgPool) {
    let app = common::app(pool.clone());
    let (a, b) = (launcher(&pool, &app).await, launcher(&pool, &app).await);
    befriend(&pool, a.user, b.user, "accepted").await;
    let (_, conv) = direct(&app, &a, b.user).await;
    let conv = conv["id"].as_str().unwrap().to_string();
    let (b2_token, b2) = add_device(&pool, &app, b.user).await;
    for i in 0..5u8 {
        let (s, _) = send(
            &app,
            &a,
            &conv,
            Uuid::now_v7(),
            vec![envelope(b.device, &[i]), envelope(b2, &[100 + i])],
        )
        .await;
        assert_eq!(s, StatusCode::ACCEPTED);
    }

    // Oldest first, paginated, this device only, with the sender's identity key.
    let (_, page) = get(&app, "/v1/inbox?limit=2", &b.token).await;
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(
        B64.decode(items[0]["ciphertext"].as_str().unwrap())
            .unwrap(),
        vec![0]
    );
    assert_eq!(
        B64.decode(items[1]["ciphertext"].as_str().unwrap())
            .unwrap(),
        vec![1]
    );
    assert_eq!(items[0]["sender_device_id"], a.device.to_string());
    assert_eq!(items[0]["sender_user_id"], a.user.to_string());
    assert_eq!(items[0]["sender_identity_key"], a.account.identity);
    assert_eq!(
        (
            items[0]["algorithm"].as_str(),
            items[0]["olm_message_type"].as_i64()
        ),
        (Some("olm.v1"), Some(1))
    );
    let cursor = page["next_cursor"].as_str().unwrap();
    let (_, rest) = get(
        &app,
        &format!("/v1/inbox?limit=10&cursor={cursor}"),
        &b.token,
    )
    .await;
    assert_eq!(rest["items"].as_array().unwrap().len(), 3);
    let (s, _) = get(&app, &format!("/v1/inbox?cursor={cursor}"), &b2_token).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "a cursor belongs to its device");

    // Acking B2's ids from B deletes nothing; B's own ids go.
    let mine: Vec<Value> = inbox(&app, &b.token)
        .await
        .iter()
        .map(|e| e["id"].clone())
        .collect();
    let theirs: Vec<Value> = inbox(&app, &b2_token)
        .await
        .iter()
        .map(|e| e["id"].clone())
        .collect();
    let resp = common::send(
        &app,
        crate::common::publishing::json_req(
            "POST",
            "/v1/inbox/ack",
            &b.token,
            &json!({"ids": theirs}),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(inbox(&app, &b2_token).await.len(), 5);
    let resp = common::send(
        &app,
        crate::common::publishing::json_req(
            "POST",
            "/v1/inbox/ack",
            &b.token,
            &json!({"ids": mine[..3]}),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(inbox(&app, &b.token).await.len(), 2);
    for bad in [
        json!({"ids": []}),
        json!({"ids": (0..501).map(|_| Uuid::now_v7()).collect::<Vec<_>>()}),
    ] {
        let resp = common::send(
            &app,
            crate::common::publishing::json_req("POST", "/v1/inbox/ack", &b.token, &bad),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // Expired envelopes disappear from the inbox and social.sweep deletes them.
    sqlx::query("UPDATE message_envelopes SET expires_at = now() - interval '1 second' WHERE recipient_device_id = $1")
        .bind(b2)
        .execute(&pool)
        .await
        .unwrap();
    assert!(inbox(&app, &b2_token).await.is_empty());
    vgames_api::social::sweep_once(&common::state(pool.clone()))
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM message_envelopes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 2);

    // No device, no inbox.
    let (_, _, bare) = seed_session(&pool, "user").await;
    assert_eq!(get(&app, "/v1/inbox", &bare).await.0, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "./migrations")]
async fn sends_are_rate_limited_per_device(pool: PgPool) {
    use vgames_api::http::ratelimit::Policy;
    let state = common::state(pool.clone());
    let app = vgames_api::http::router(state.clone());
    let (a, b) = (launcher(&pool, &app).await, launcher(&pool, &app).await);
    befriend(&pool, a.user, b.user, "accepted").await;
    let (_, conv) = direct(&app, &a, b.user).await;
    let conv = conv["id"].as_str().unwrap().to_string();
    let (s, _) = send(
        &app,
        &a,
        &conv,
        Uuid::now_v7(),
        vec![envelope(b.device, b"x")],
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    // Spend the rest of this device's 120-per-minute budget at once: sending 120 real
    // requests would race the refill (one send every 0.5 s).
    let key = format!("send:{}", a.device);
    let mut spent = 0;
    while state.limits.check(Policy::Messages, &key).is_ok() {
        spent += 1;
        assert!(spent <= 200, "the send budget never runs out");
    }
    assert!(spent >= 100, "only {spent} sends were left after one");
    let (s, e) = send(
        &app,
        &a,
        &conv,
        Uuid::now_v7(),
        vec![envelope(b.device, b"x")],
    )
    .await;
    assert_eq!(
        (s, e["code"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    // Another device of the same user has its own budget.
    let (token, device) = add_device(&pool, &app, a.user).await;
    let other = Launcher {
        user: a.user,
        token,
        account: Account::new(),
        device,
    };
    assert_eq!(
        send(
            &app,
            &other,
            &conv,
            Uuid::now_v7(),
            vec![envelope(b.device, b"x")]
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
}

/// A real Olm conversation through the relay, then a scan of every column of every table:
/// the plaintext never reaches the server in any form.
#[sqlx::test(migrations = "./migrations")]
async fn plaintext_never_reaches_the_database(pool: PgPool) {
    use vodozemac::olm::{Account as OlmAccount, OlmMessage, SessionConfig};

    let app = common::app(pool.clone());
    let (ua, _, ta) = seed_session(&pool, "user").await;
    let (ub, _, tb) = seed_session(&pool, "user").await;
    befriend(&pool, ua, ub, "accepted").await;
    let register = |acc: &OlmAccount, user: Uuid| {
        let identity = acc.curve25519_key().to_base64();
        let signing = acc.ed25519_key().to_base64();
        let sig = acc
            .sign(canonical::device_keys(
                &identity,
                server_id(),
                &signing,
                user,
            ))
            .to_base64();
        json!({"display_name": "Olm", "platform": "linux", "identity_key": identity, "signing_key": signing, "keys_signature": sig})
    };
    let alice = OlmAccount::new();
    let mut bob = OlmAccount::new();
    let (_, da) = post(&app, "/v1/devices", &ta, &register(&alice, ua)).await;
    let (_, db) = post(&app, "/v1/devices", &tb, &register(&bob, ub)).await;
    let db: Uuid = db["id"].as_str().unwrap().parse().unwrap();
    let _ = da;
    bob.generate_one_time_keys(5);
    let otks: Vec<Value> = bob
        .one_time_keys()
        .into_iter()
        .map(|(id, key)| {
            let (id, key) = (id.to_base64(), key.to_base64());
            let sig = bob
                .sign(canonical::one_time_key(false, &key, &id))
                .to_base64();
            json!({"key_id": id, "public_key": key, "signature": sig})
        })
        .collect();
    let (s, _) = post(
        &app,
        &format!("/v1/devices/{db}/one-time-keys"),
        &tb,
        &json!({"one_time_keys": otks}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    bob.mark_keys_as_published();

    // Alice claims a key, starts a session and sends the canary.
    let (_, claimed) = post(&app, "/v1/keys/claim", &ta, &json!({"device_ids": [db]})).await;
    let otk = vodozemac::Curve25519PublicKey::from_base64(
        claimed["items"][0]["public_key"].as_str().unwrap(),
    )
    .unwrap();
    let mut session = alice
        .create_outbound_session(SessionConfig::version_1(), bob.curve25519_key(), otk)
        .unwrap();
    let canary = "CANARY-plaintext-4f1c9a said gg";
    let (_, conv) = post(
        &app,
        "/v1/conversations",
        &ta,
        &json!({"kind": "direct", "user_id": ub}),
    )
    .await;
    let conv = conv["id"].as_str().unwrap().to_string();
    let (kind, bytes) = session.encrypt(canary).unwrap().to_parts();
    let body = json!({"client_message_id": Uuid::now_v7(), "envelopes": [
        {"recipient_device_id": db, "olm_message_type": kind, "ciphertext": B64.encode(&bytes)}
    ]});
    let (s, r) = post(
        &app,
        &format!("/v1/conversations/{conv}/messages"),
        &ta,
        &body,
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{r}");

    // Every column of every table, as text (bytea as hex), against the canary's encodings.
    let needles = [
        canary.to_string(),
        hex::encode(canary.as_bytes()),
        B64.encode(canary.as_bytes()),
        "CANARY".to_string(),
    ];
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT quote_ident(table_name) FROM information_schema.tables WHERE table_schema = 'public' AND table_type = 'BASE TABLE'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(tables.iter().any(|t| t == "message_envelopes"));
    let mut rows_seen = 0usize;
    for t in &tables {
        let rows: Vec<String> =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT t::text FROM {t} t")))
                .fetch_all(&pool)
                .await
                .unwrap();
        for row in rows {
            rows_seen += 1;
            for n in &needles {
                assert!(
                    !row.to_lowercase().contains(&n.to_lowercase()),
                    "plaintext found in {t}: {row}"
                );
            }
        }
    }
    assert!(rows_seen > 10);

    // Bob decrypts it from his inbox: what the server relayed was the real message.
    let items = inbox(&app, &tb).await;
    assert_eq!(items.len(), 1);
    let bytes = B64
        .decode(items[0]["ciphertext"].as_str().unwrap())
        .unwrap();
    let message = OlmMessage::from_parts(
        usize::try_from(items[0]["olm_message_type"].as_u64().unwrap()).unwrap(),
        &bytes,
    )
    .unwrap();
    let OlmMessage::PreKey(pre_key) = message else {
        panic!("expected a pre-key message")
    };
    let sender = vodozemac::Curve25519PublicKey::from_base64(
        items[0]["sender_identity_key"].as_str().unwrap(),
    )
    .unwrap();
    let inbound = bob
        .create_inbound_session(SessionConfig::version_1(), sender, &pre_key)
        .unwrap();
    assert_eq!(inbound.plaintext, canary.as_bytes());
}
