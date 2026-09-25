//! Finalize and re-sign (A1-T11 part 2): a real package built by `vgames-pack`, uploaded
//! through the fs backend and signed with test publisher keys.
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
use serde_json::json;
use sqlx::PgPool;
use vgames_api::{storage::BucketKind, versions::signature_object};

#[sqlx::test(migrations = "./migrations")]
async fn a_real_package_finalizes_and_can_be_re_signed(pool: PgPool) {
    let w = world(&pool).await;
    let (version, built) = full_upload(&w, false).await;

    let (status, v) = finalize(
        &w,
        version,
        &built.manifest,
        envelope(&key(2), &built.manifest),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["state"], "verifying");
    assert_eq!(v["pack_count"], built.packs.len());
    assert_eq!(v["file_count"], 3);
    assert_eq!(v["total_size"], 500_005);
    assert_eq!(
        v["publisher_key_id"],
        key(2).public_key().key_id().to_string()
    );
    assert!(v["finalized_at"].is_string());

    let packs: i64 = sqlx::query_scalar("SELECT count(*) FROM package_packs WHERE version_id = $1")
        .bind(version)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(packs as usize, built.packs.len());
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'version.verify'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(jobs, 1);
    assert!(
        w.state
            .storage
            .head(BucketKind::Packages, &signature_object(w.package, version))
            .await
            .unwrap()
            .is_some()
    );

    // Finalizing twice is a conflict.
    let (status, body) = finalize(
        &w,
        version,
        &built.manifest,
        envelope(&key(2), &built.manifest),
    )
    .await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::CONFLICT, "version_not_uploading")
    );

    // Re-sign with another of the owner's keys; the manifest bytes stay the same.
    let uri = format!("/v1/admin/versions/{version}/signature");
    let resp = send(
        &w.app,
        json_req(
            "POST",
            &uri,
            &w.owner,
            &json!({ "signature": envelope(&key(6), &built.manifest) }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        body_json(resp).await["publisher_key_id"],
        key(6).public_key().key_id().to_string()
    );
    let resp = send(
        &w.app,
        json_req(
            "POST",
            &uri,
            &w.owner,
            &json!({ "signature": envelope(&key(6), b"other bytes") }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(code(&body_json(resp).await), "manifest_hash_mismatch");
    let resp = send(
        &w.app,
        json_req(
            "POST",
            &uri,
            &w.owner,
            &json!({ "signature": envelope(&key(3), &built.manifest) }),
        ),
    )
    .await;
    assert_eq!(code(&body_json(resp).await), "publisher_key_not_yours");
}

#[sqlx::test(migrations = "./migrations")]
async fn every_failure_has_its_own_code(pool: PgPool) {
    let w = world(&pool).await;

    let (version, built) = full_upload(&w, false).await;
    let m = &built.manifest;
    let mut tampered = envelope(&key(2), m);
    let mut sig = STANDARD
        .decode(tampered["signature"].as_str().unwrap())
        .unwrap();
    sig[0] ^= 1;
    tampered["signature"] = json!(STANDARD.encode(sig));
    let mut wrong_format = envelope(&key(2), m);
    wrong_format["format"] = json!("vgames.sig/2");
    // Missing fields are a request-shape error, not a signature one.
    let (status, body) = finalize(&w, version, m, json!({ "format": "vgames.sig/1" })).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::BAD_REQUEST, "validation_failed")
    );
    for (signature, expected) in [
        (envelope(&key(9), m), "publisher_key_untrusted"),
        (envelope(&key(5), m), "publisher_key_untrusted"),
        (envelope(&key(3), m), "publisher_key_not_yours"),
        (envelope(&key(4), m), "publisher_key_expired"),
        (tampered, "signature_invalid"),
        (wrong_format, "signature_invalid"),
    ] {
        let (status, body) = finalize(&w, version, m, signature).await;
        assert_eq!(
            (status, code(&body)),
            (StatusCode::UNPROCESSABLE_ENTITY, expected),
            "{body}"
        );
    }
    // Declared hash differs from the uploaded bytes.
    let mut other = m.clone();
    other.push(b' ');
    let (status, body) = finalize(&w, version, &other, envelope(&key(2), &other)).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::UNPROCESSABLE_ENTITY, "manifest_hash_mismatch"),
        "{body}"
    );

    // The manifest is signed but not a valid manifest.
    let (version, _) = new_version(&w, false).await;
    upload_manifest(&w, version, b"{\"format\":\"nope\"}".to_vec()).await;
    let bad = b"{\"format\":\"nope\"}";
    let (status, body) = finalize(&w, version, bad, envelope(&key(2), bad)).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::UNPROCESSABLE_ENTITY, "manifest_invalid"),
        "{body}"
    );
    assert_eq!(body["errors"][0]["field"], "manifest");

    // The manifest names another sequence.
    let (version, built) = full_upload(&w, true).await;
    let (status, body) = finalize(
        &w,
        version,
        &built.manifest,
        envelope(&key(2), &built.manifest),
    )
    .await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::UNPROCESSABLE_ENTITY, "manifest_mismatch"),
        "{body}"
    );
    assert_eq!(body["errors"][0]["field"], "sequence");

    // A pack is missing, then present with the wrong size.
    let (version, identity) = new_version(&w, false).await;
    let built = build(&identity);
    upload_manifest(&w, version, built.manifest.clone()).await;
    let (status, body) = finalize(
        &w,
        version,
        &built.manifest,
        envelope(&key(2), &built.manifest),
    )
    .await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::UNPROCESSABLE_ENTITY, "pack_missing"),
        "{body}"
    );
    assert_eq!(body["errors"][0]["field"], "packs[0]");
    let mut short = built.packs[0].clone();
    short.pop();
    upload_pack(&w, version, 0, short).await;
    let (status, body) = finalize(
        &w,
        version,
        &built.manifest,
        envelope(&key(2), &built.manifest),
    )
    .await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::UNPROCESSABLE_ENTITY, "pack_size_mismatch"),
        "{body}"
    );

    // Nothing was recorded for failed attempts.
    let states: Vec<String> = sqlx::query_scalar("SELECT DISTINCT state FROM package_versions")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(states, ["uploading"]);
}
