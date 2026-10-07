//! Signed compatibility profiles (A1-T12 part 3).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use crate::common;

use axum::http::StatusCode;
use base64::{Engine, engine::general_purpose::STANDARD};
use common::{publishing::*, *};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use vgames_core::{Context, Envelope, SecretKey};

/// A `vgames.compat/1` document (shape from vgames-core's wire-format snapshot).
fn document(package: Uuid, target: &str, revision: u64) -> Vec<u8> {
    let mut v = json!({
        "format": "vgames.compat/1",
        "server_id": SERVER,
        "package_id": package,
        "target": target,
        "revision": revision,
        "created_at": "2026-09-24T10:00:00Z",
        "applies_to": { "platform": "windows-x86_64", "min_sequence": 1, "max_sequence": null },
        "status": "verified",
        "notes": "Controller works out of the box.",
        "runner": {
            "kind": "proton",
            "prefer": ["umu-proton", "ge-proton"],
            "min_version": "GE-Proton10-1",
            "umu_game_id": "umu-12345",
            "env": { "PROTON_ENABLE_NVAPI": "1" },
            "dll_overrides": { "d3dcompiler_47": "native,builtin" },
            "winetricks": ["vcrun2022"]
        }
    });
    if target == "macos" {
        // Same adjustments as vgames-core's own macOS test profile.
        v["runner"]["kind"] = json!("wine");
        v["runner"]["prefer"] = json!(["wine-gcenx"]);
        v["runner"]["min_version"] = json!("10.0");
        v["runner"]["graphics"] = json!(["d3dmetal", "dxmt", "dxvk", "wined3d"]);
        v["runner"].as_object_mut().unwrap().remove("umu_game_id");
    }
    serde_json::to_vec(&v).unwrap()
}

fn signed(signer: &SecretKey, doc: &[u8]) -> Value {
    let env: Value =
        serde_json::from_slice(&Envelope::sign(signer, Context::Compat, doc).to_bytes()).unwrap();
    json!({ "document": STANDARD.encode(doc), "signature": env })
}

async fn put(w: &World, target: &str, body: &Value) -> (StatusCode, Value) {
    let uri = format!("/v1/admin/packages/{}/compat/{target}", w.package);
    let resp = send(&w.app, json_req("PUT", &uri, &w.owner, body)).await;
    let status = resp.status();
    (status, body_json(resp).await)
}

fn code(v: &Value) -> Option<&str> {
    v["code"].as_str()
}

#[sqlx::test(migrations = "./migrations")]
async fn profiles_are_verified_versioned_and_listed(pool: PgPool) {
    let w = world(&pool).await;
    let pkg = w.package;

    let (status, stored) = put(&w, "linux", &signed(&key(2), &document(pkg, "linux", 1))).await;
    assert_eq!(status, StatusCode::CREATED, "{stored}");
    assert_eq!(
        (
            stored["target"].as_str(),
            stored["revision"].as_i64(),
            stored["status"].as_str()
        ),
        (Some("linux"), Some(1), Some("verified"))
    );

    // Distinct 4xx codes for each way a profile can be wrong.
    let cases = [
        (
            "linux",
            signed(&key(2), &document(pkg, "linux", 1)),
            StatusCode::CONFLICT,
            "stale_revision",
        ),
        (
            "linux",
            signed(&key(2), &document(pkg, "macos", 2)),
            StatusCode::UNPROCESSABLE_ENTITY,
            "compat_mismatch",
        ),
        (
            "linux",
            signed(&key(2), &document(Uuid::now_v7(), "linux", 2)),
            StatusCode::UNPROCESSABLE_ENTITY,
            "compat_mismatch",
        ),
        (
            "linux",
            signed(&key(3), &document(pkg, "linux", 2)),
            StatusCode::UNPROCESSABLE_ENTITY,
            "publisher_key_not_yours",
        ),
        (
            "linux",
            signed(&key(5), &document(pkg, "linux", 2)),
            StatusCode::UNPROCESSABLE_ENTITY,
            "publisher_key_untrusted",
        ),
        (
            "linux",
            signed(&key(4), &document(pkg, "linux", 2)),
            StatusCode::UNPROCESSABLE_ENTITY,
            "publisher_key_expired",
        ),
        (
            "linux",
            signed(&key(2), b"{\"format\":\"vgames.compat/1\"}"),
            StatusCode::UNPROCESSABLE_ENTITY,
            "compat_invalid",
        ),
    ];
    for (target, body, status, expected) in cases {
        let (got, resp) = put(&w, target, &body).await;
        assert_eq!((got, code(&resp)), (status, Some(expected)), "{resp}");
    }
    let mut tampered = signed(&key(2), &document(pkg, "linux", 2));
    tampered["document"] = json!(STANDARD.encode(document(pkg, "linux", 3)));
    assert_eq!(
        code(&put(&w, "linux", &tampered).await.1),
        Some("manifest_hash_mismatch")
    );
    let uri = format!("/v1/admin/packages/{pkg}/compat/windows");
    let resp = send(
        &w.app,
        json_req(
            "PUT",
            &uri,
            &w.owner,
            &signed(&key(2), &document(pkg, "linux", 2)),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    assert_eq!(
        put(&w, "linux", &signed(&key(2), &document(pkg, "linux", 2)))
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        put(&w, "macos", &signed(&key(6), &document(pkg, "macos", 1)))
            .await
            .0,
        StatusCode::CREATED
    );

    // Players see the latest revision per target once the package is published.
    let (_, _, player) = seed_session(&pool, "user").await;
    let uri = format!("/v1/packages/{pkg}/compat");
    assert_eq!(
        send(&w.app, bearer_request("GET", &uri, &player))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE packages SET status = 'published' WHERE id = $1")
        .bind(pkg)
        .execute(&pool)
        .await
        .unwrap();
    let list = body_json(send(&w.app, bearer_request("GET", &uri, &player)).await).await;
    let items = list["items"].as_array().unwrap();
    let got: Vec<(&str, i64)> = items
        .iter()
        .map(|p| {
            (
                p["target"].as_str().unwrap(),
                p["revision"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(got, [("linux", 2), ("macos", 1)]);
    // What the launcher receives verifies.
    for (item, signer) in items.iter().zip([key(2), key(6)]) {
        let doc = STANDARD.decode(item["document"].as_str().unwrap()).unwrap();
        let env = Envelope::parse(&serde_json::to_vec(&item["signature"]).unwrap()).unwrap();
        env.verify(&signer.public_key(), Context::Compat, &doc)
            .unwrap();
    }
    let audit: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'compat.publish'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audit, 3);
}

#[sqlx::test(migrations = "./migrations")]
async fn admin_history_pages_signed_revisions_of_unpublished_packages(pool: PgPool) {
    let w = world(&pool).await;
    for revision in 1..=3 {
        assert_eq!(
            put(
                &w,
                "linux",
                &signed(&key(2), &document(w.package, "linux", revision))
            )
            .await
            .0,
            StatusCode::CREATED
        );
    }
    let uri = format!("/v1/admin/packages/{}/compat/linux", w.package);
    let resp = send(
        &w.app,
        bearer_request("GET", &format!("{uri}?limit=2"), &w.owner),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let first = body_json(resp).await;
    let items = first["items"].as_array().unwrap();
    assert_eq!(
        items
            .iter()
            .map(|i| i["revision"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [3, 2]
    );
    for item in items {
        assert!(item["created_at"].as_str().is_some());
        let bytes = STANDARD.decode(item["document"].as_str().unwrap()).unwrap();
        let envelope = Envelope::parse(&serde_json::to_vec(&item["signature"]).unwrap()).unwrap();
        envelope
            .verify(&key(2).public_key(), Context::Compat, &bytes)
            .unwrap();
        assert_eq!(
            item["signature"]["key_id"],
            key(2).public_key().key_id().to_string()
        );
    }
    let cursor = first["next_cursor"].as_str().unwrap();
    let resp = send(
        &w.app,
        bearer_request("GET", &format!("{uri}?limit=2&cursor={cursor}"), &w.owner),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let last = body_json(resp).await;
    assert_eq!(last["items"].as_array().unwrap().len(), 1);
    assert_eq!(last["items"][0]["revision"], 1);
    assert!(last.get("next_cursor").is_none());

    let macos = format!("/v1/admin/packages/{}/compat/macos", w.package);
    let empty = body_json(send(&w.app, bearer_request("GET", &macos, &w.owner)).await).await;
    assert_eq!(empty["items"], json!([]));
    for bad in [
        format!("{uri}?cursor={cursor}A"),
        format!("{macos}?cursor={cursor}"),
    ] {
        let resp = send(&w.app, bearer_request("GET", &bad, &w.owner)).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(resp).await["code"], "invalid_cursor");
    }
    let created = send(
        &w.app,
        json_req(
            "POST",
            "/v1/admin/packages",
            &w.owner,
            &json!({ "title": "Cursor scope", "fetch_metadata": false }),
        ),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let other = body_json(created).await;
    let other_uri = format!(
        "/v1/admin/packages/{}/compat/linux?cursor={cursor}",
        other["id"].as_str().unwrap()
    );
    assert_eq!(
        send(&w.app, bearer_request("GET", &other_uri, &w.owner))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let (_, _, player) = seed_session(&pool, "user").await;
    assert_eq!(
        send(&w.app, bearer_request("GET", &uri, &player))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&w.app, get_req(&uri)).await.status(),
        StatusCode::UNAUTHORIZED
    );
    for (path, status) in [
        (format!("{uri}?limit=201"), StatusCode::BAD_REQUEST),
        (
            format!("/v1/admin/packages/{}/compat/windows", w.package),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("/v1/admin/packages/{}/compat/linux", Uuid::now_v7()),
            StatusCode::NOT_FOUND,
        ),
    ] {
        assert_eq!(
            send(&w.app, bearer_request("GET", &path, &w.owner))
                .await
                .status(),
            status
        );
    }
}
