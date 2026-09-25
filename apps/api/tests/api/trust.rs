//! Trust bundle upload and publisher keys (A1-T08), with bundles signed by test keys.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::common::*;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use vgames_core::{
    SecretKey, Timestamp,
    trust::{FORMAT, NextRoot, PublisherKey, Revocation, TrustBundle, sign_bundle},
};

const SERVER: &str = "01920000-0000-7000-8000-00000000abcd";

fn key(n: u8) -> SecretKey {
    SecretKey::from_seed(&[n; 32])
}

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn publisher(k: &SecretKey, holder: Uuid, label: &str) -> PublisherKey {
    PublisherKey {
        key_id: k.public_key().key_id(),
        public_key: k.public_key(),
        holder_user_id: holder,
        label: label.into(),
        not_before: ts("2026-09-24T00:00:00Z"),
        not_after: ts("2028-09-24T00:00:00Z"),
    }
}

fn bundle(root: &SecretKey, version: u64, publishers: Vec<PublisherKey>) -> TrustBundle {
    TrustBundle {
        format: FORMAT.into(),
        server_id: SERVER.parse().unwrap(),
        version,
        issued_at: ts("2026-09-24T10:00:00Z"),
        expires_at: Some(ts("2027-09-24T10:00:00Z")),
        root_key_id: root.public_key().key_id(),
        publishers,
        revoked: vec![],
        next_root: None,
    }
}

/// The API body for a bundle signed by `signer`.
fn signed(signer: &SecretKey, b: &TrustBundle) -> Value {
    let bytes = b.to_bytes();
    let sig = sign_bundle(signer, &bytes);
    json!({ "bundle": STANDARD.encode(&bytes), "signature": sig.to_base64() })
}

/// An app whose configured root is `key(1)`.
fn app_with_root(pool: PgPool) -> Router {
    let config = config_with(&[("VGAMES_ROOT_PUBLIC_KEY", &key(1).public_key().to_base64())]);
    vgames_api::http::router(vgames_api::AppState::new(config, pool).unwrap())
}

async fn upload(app: &Router, token: &str, body: &Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/admin/trust/bundles")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap();
    let resp = send(app, req).await;
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn publisher_keys(app: &Router, token: &str) -> Value {
    let resp = send(
        app,
        bearer_request("GET", "/v1/admin/trust/publisher-keys", token),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp).await
}

#[sqlx::test(migrations = "./migrations")]
async fn owner_uploads_and_everyone_fetches_the_bundle(pool: PgPool) {
    let app = app_with_root(pool.clone());
    let (_, _, owner) = seed_session(&pool, "owner").await;
    let (admin_id, _, admin) = seed_session(&pool, "admin").await;
    let (_, _, user) = seed_session(&pool, "user").await;

    assert_eq!(
        send(&app, get_req("/v1/trust/bundle")).await.status(),
        StatusCode::NOT_FOUND
    );
    let empty = publisher_keys(&app, &admin).await;
    assert_eq!(empty["bundle_version"], 0);
    assert!(empty["items"].as_array().unwrap().is_empty());

    let b = bundle(
        &key(1),
        1,
        vec![publisher(&key(2), admin_id, "alice@workstation")],
    );
    let body = signed(&key(1), &b);
    for token in [&admin, &user] {
        assert_eq!(upload(&app, token, &body).await.0, StatusCode::FORBIDDEN);
    }
    let (status, created) = upload(&app, &owner, &body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created, json!({ "version": 1 }));

    // Served verbatim, without authentication, cacheable for a minute.
    let resp = send(&app, get_req("/v1/trust/bundle")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CACHE_CONTROL], "public, max-age=60");
    assert_eq!(body_json(resp).await, body);

    let keys = publisher_keys(&app, &admin).await;
    assert_eq!(keys["bundle_version"], 1);
    assert_eq!(keys["bundle_expires_at"], "2027-09-24T10:00:00Z");
    let item = &keys["items"][0];
    assert_eq!(item["key_id"], key(2).public_key().key_id().to_string());
    assert_eq!(item["public_key"], key(2).public_key().to_base64());
    assert_eq!(item["holder"]["id"], admin_id.to_string());
    assert_eq!(item["not_after"], "2028-09-24T00:00:00Z");
    assert_eq!(
        send(
            &app,
            bearer_request("GET", "/v1/admin/trust/publisher-keys", &user)
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );

    let audit: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'trust.bundle_upload'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audit, 1);

    // The server is reachable by fingerprint now.
    let info = body_json(send(&app, get_req("/.well-known/vgames.json")).await).await;
    assert_eq!(
        info["root_key_fingerprint"],
        key(1).public_key().fingerprint().to_string()
    );
    assert_eq!(info["root_public_key"], key(1).public_key().to_base64());
}

#[sqlx::test(migrations = "./migrations")]
async fn bad_bundles_are_refused(pool: PgPool) {
    let app = app_with_root(pool.clone());
    let (owner_id, _, owner) = seed_session(&pool, "owner").await;
    let (_, _, _) = seed_session(&pool, "admin").await;
    let (user_id, _, _) = seed_session(&pool, "user").await;
    let good = |v| bundle(&key(1), v, vec![publisher(&key(2), owner_id, "owner key")]);

    // Signed by a key that is not the configured root.
    let (status, body) = upload(&app, &owner, &signed(&key(9), &bundle(&key(9), 1, vec![]))).await;
    assert_eq!(
        (status, body["type"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("urn:vgames:problem:bad_signature")
        )
    );

    // Bytes changed after signing.
    let mut tampered = signed(&key(1), &good(1));
    let mut bytes = good(1).to_bytes();
    bytes.push(b' ');
    tampered["bundle"] = json!(STANDARD.encode(&bytes));
    assert_eq!(
        upload(&app, &owner, &tampered).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    // Another server's bundle.
    let mut other = good(1);
    other.server_id = Uuid::now_v7();
    let (status, body) = upload(&app, &owner, &signed(&key(1), &other)).await;
    assert_eq!(
        (status, body["type"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("urn:vgames:problem:wrong_server")
        )
    );

    // Holders must be admins of this server.
    let mut strangers = good(1);
    strangers
        .publishers
        .push(publisher(&key(3), user_id, "plain user"));
    strangers
        .publishers
        .push(publisher(&key(4), Uuid::now_v7(), "nobody"));
    let (status, body) = upload(&app, &owner, &signed(&key(1), &strangers)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let fields: Vec<&str> = body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["field"].as_str().unwrap())
        .collect();
    assert_eq!(
        fields,
        [
            "publishers[1].holder_user_id",
            "publishers[2].holder_user_id"
        ]
    );

    // Malformed transport.
    let (status, _) = upload(
        &app,
        &owner,
        &json!({ "bundle": "%%%", "signature": "AA==" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = upload(
        &app,
        &owner,
        &json!({ "bundle": "e30=", "signature": "AA==", "extra": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let garbage = sign_bundle(&key(1), b"not json");
    let body = json!({ "bundle": STANDARD.encode(b"not json"), "signature": garbage.to_base64() });
    let (status, body) = upload(&app, &owner, &body).await;
    assert_eq!(
        (status, body["type"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("urn:vgames:problem:invalid_bundle")
        )
    );

    // Nothing was stored; then versions only go up.
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM trust_bundles")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        upload(&app, &owner, &signed(&key(1), &good(2))).await.0,
        StatusCode::CREATED
    );
    let (status, body) = upload(&app, &owner, &signed(&key(1), &good(2))).await;
    assert_eq!(
        (status, body["type"].as_str()),
        (
            StatusCode::CONFLICT,
            Some("urn:vgames:problem:stale_version")
        )
    );
    assert_eq!(
        upload(&app, &owner, &signed(&key(1), &good(1))).await.0,
        StatusCode::CONFLICT
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn root_rotation_and_revocation(pool: PgPool) {
    let app = app_with_root(pool.clone());
    let (owner_id, _, owner) = seed_session(&pool, "owner").await;
    let (alice, bob, carol) = (key(2), key(3), key(4));
    let old_root = key(1);
    let new_root = key(5);

    // v1 (old root) announces the next root.
    let mut v1 = bundle(
        &old_root,
        1,
        vec![
            publisher(&alice, owner_id, "alice"),
            publisher(&bob, owner_id, "bob"),
        ],
    );
    v1.next_root = Some(NextRoot {
        public_key: new_root.public_key(),
        key_id: new_root.public_key().key_id(),
    });
    assert_eq!(
        upload(&app, &owner, &signed(&old_root, &v1)).await.0,
        StatusCode::CREATED
    );

    // v2 is signed by the new root: bob is revoked, carol added.
    let mut v2 = bundle(
        &new_root,
        2,
        vec![
            publisher(&alice, owner_id, "alice"),
            publisher(&carol, owner_id, "carol"),
        ],
    );
    v2.revoked.push(Revocation {
        key_id: bob.public_key().key_id(),
        revoked_at: ts("2026-10-01T12:00:00Z"),
        reason: "laptop stolen".into(),
    });
    let (status, body) = upload(&app, &owner, &signed(&new_root, &v2)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let keys = publisher_keys(&app, &owner).await;
    let labels: Vec<(&str, Option<&str>)> = keys["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| {
            (
                k["label"].as_str().unwrap(),
                k["revocation_reason"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        labels,
        [
            ("alice", None),
            ("bob", Some("laptop stolen")),
            ("carol", None)
        ]
    );

    // After the rotation the old root can no longer sign.
    let v3 = bundle(&old_root, 3, vec![publisher(&alice, owner_id, "alice")]);
    let (status, body) = upload(&app, &owner, &signed(&old_root, &v3)).await;
    assert_eq!(
        (status, body["type"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("urn:vgames:problem:bad_signature")
        )
    );

    // v3 by the new root drops carol: she is no longer listed, but her row survives for old versions.
    let v3 = bundle(&new_root, 3, vec![publisher(&alice, owner_id, "alice")]);
    assert_eq!(
        upload(&app, &owner, &signed(&new_root, &v3)).await.0,
        StatusCode::CREATED
    );
    let keys = publisher_keys(&app, &owner).await;
    assert_eq!(keys["bundle_version"], 3);
    assert_eq!(keys["items"].as_array().unwrap().len(), 1);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM publisher_keys")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 3);
}
