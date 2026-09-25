//! Trust bundles and publisher keys (A1-T08, docs/architecture/01-security.md §3.2).
//!
//! Every check on a bundle is `vgames_core::trust::verify_bundle` (Agent 5). This module
//! chooses the root pin, enforces what only the server knows (version order, holders),
//! stores the exact bytes and materializes `publisher_keys` for upload-time checks.

use std::collections::HashSet;

use axum::{
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;
use time::OffsetDateTime;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_core::{
    PublicKey, Signature, Timestamp,
    trust::{self, RootPin, TrustError},
};
use vgames_proto::{
    common::FieldError,
    trust::{PublisherKey, PublisherKeyList, SignedTrustBundle, TrustBundleStored},
};

use crate::{
    audit,
    auth::{RequestMeta, RequireAdmin, RequireOwner},
    error::{ApiError, ApiResult},
    http::json::{Json, Validate},
    openapi_problems::{BadRequest, Conflict, Forbidden, NotFound, Unauthorized, Unprocessable},
    packages,
    state::AppState,
};

/// Serializes bundle uploads (`pg_advisory_xact_lock` key).
const UPLOAD_LOCK: i64 = 0x7667_7472_7573_7401;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_bundle))
        .routes(routes!(upload_bundle))
        .routes(routes!(list_publisher_keys))
}

/// The pin new uploads must verify under: the configured root, moved along every stored
/// bundle that verifies under the pin so far (so a completed `next_root` rotation carries
/// over, and an operator who already switched `VGAMES_ROOT_PUBLIC_KEY` to the new root is
/// not bound to bundles signed by the old one). `stored` is in ascending version order.
fn current_pin(root: PublicKey, stored: &[(Vec<u8>, Vec<u8>)], server_id: Uuid) -> RootPin {
    let mut pin = RootPin::new(root);
    for (bytes, sig) in stored {
        let Ok(sig) = <[u8; 64]>::try_from(sig.as_slice()) else {
            continue;
        };
        if let Ok(v) =
            trust::verify_bundle(bytes, &Signature::from_bytes(sig), &pin, None, server_id)
        {
            pin = v.pin;
        }
    }
    pin
}

/// The trust state of the latest stored bundle, verified along the same pin chain as
/// uploads; `None` while no bundle verifies.
pub async fn current_state(state: &AppState) -> ApiResult<Option<trust::TrustState>> {
    let stored = sqlx::query!("SELECT bundle, signature FROM trust_bundles ORDER BY version")
        .fetch_all(&state.db)
        .await?;
    let server_id = state.config.server_id;
    let mut pin = RootPin::new(state.config.root_public_key);
    let mut latest = None;
    for row in stored {
        let Ok(sig) = <[u8; 64]>::try_from(row.signature.as_slice()) else {
            continue;
        };
        if let Ok(v) = trust::verify_bundle(
            &row.bundle,
            &Signature::from_bytes(sig),
            &pin,
            None,
            server_id,
        ) {
            pin = v.pin;
            latest = Some(v.state);
        }
    }
    Ok(latest)
}

fn trust_error(e: TrustError) -> ApiError {
    let detail = e.to_string();
    match e {
        TrustError::TooLarge => ApiError::field("bundle", "too_large", detail),
        TrustError::Rollback { .. } => {
            ApiError::conflict("stale_version", "A newer trust bundle is already stored")
                .with_detail(detail)
        }
        TrustError::Signature | TrustError::RootKeyId => ApiError::unprocessable(
            "bad_signature",
            "The bundle is not signed by this server's root key",
        )
        .with_detail(detail),
        TrustError::ServerId { .. } => {
            ApiError::unprocessable("wrong_server", "The bundle is for another server")
                .with_detail(detail)
        }
        _ => ApiError::unprocessable("invalid_bundle", "The trust bundle is invalid")
            .with_detail(detail),
    }
}

fn to_offset(t: Timestamp) -> ApiResult<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp(t.unix()).map_err(ApiError::internal_from)
}

/// Latest root-signed trust bundle
#[utoipa::path(
    get,
    path = "/v1/trust/bundle",
    tag = "trust",
    operation_id = "getTrustBundle",
    security(()),
    responses((status = 200, description = "Bundle bytes and root signature", body = SignedTrustBundle), NotFound)
)]
pub async fn get_bundle(State(state): State<AppState>) -> ApiResult<Response> {
    let row =
        sqlx::query!("SELECT bundle, signature FROM trust_bundles ORDER BY version DESC LIMIT 1")
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(ApiError::not_found)?;
    let body = SignedTrustBundle {
        bundle: STANDARD.encode(&row.bundle),
        signature: STANDARD.encode(&row.signature),
    };
    Ok((
        [(header::CACHE_CONTROL, "public, max-age=60")],
        axum::Json(body),
    )
        .into_response())
}

impl Validate for SignedTrustBundle {
    fn validate(&self, _errors: &mut Vec<FieldError>) {}
}

/// Upload a new root-signed trust bundle (owner only)
#[utoipa::path(
    post,
    path = "/v1/admin/trust/bundles",
    tag = "admin-server",
    operation_id = "adminUploadTrustBundle",
    request_body = SignedTrustBundle,
    responses(
        (status = 201, description = "Stored; publisher keys rebuilt", body = inline(TrustBundleStored)),
        BadRequest, Unauthorized, Forbidden, Conflict, Unprocessable
    )
)]
pub async fn upload_bundle(
    State(state): State<AppState>,
    RequireOwner(owner): RequireOwner,
    meta: RequestMeta,
    Json(req): Json<SignedTrustBundle>,
) -> ApiResult<Response> {
    if req.bundle.len() > trust::MAX_BUNDLE_BYTES.div_ceil(3) * 4 {
        return Err(trust_error(TrustError::TooLarge));
    }
    let bytes = STANDARD
        .decode(req.bundle.as_bytes())
        .map_err(|_| ApiError::field("bundle", "base64", "must be standard base64"))?;
    let signature = STANDARD
        .decode(req.signature.as_bytes())
        .ok()
        .and_then(|s| <[u8; 64]>::try_from(s.as_slice()).ok())
        .map(Signature::from_bytes)
        .ok_or_else(|| {
            ApiError::field("signature", "base64", "must be standard base64 of 64 bytes")
        })?;

    let mut tx = state.db.begin().await?;
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", UPLOAD_LOCK)
        .execute(&mut *tx)
        .await?;
    let stored: Vec<(Vec<u8>, Vec<u8>)> =
        sqlx::query!("SELECT bundle, signature FROM trust_bundles ORDER BY version")
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|r| (r.bundle, r.signature))
            .collect();
    let last_seen = sqlx::query_scalar!("SELECT max(version) FROM trust_bundles")
        .fetch_one(&mut *tx)
        .await?
        .and_then(|v| u64::try_from(v).ok());
    let server_id = state.config.server_id;
    let pin = current_pin(state.config.root_public_key, &stored, server_id);
    let verified = trust::verify_bundle(&bytes, &signature, &pin, last_seen, server_id)
        .map_err(trust_error)?;
    if !verified.newer {
        return Err(ApiError::conflict(
            "stale_version",
            "A trust bundle with this version is already stored",
        ));
    }
    let bundle = verified.state.bundle();
    let version = i64::try_from(bundle.version).map_err(|_| {
        ApiError::unprocessable("invalid_bundle", "The bundle version is too large")
    })?;

    // Publisher keys sign packages for this server: each holder must be one of its admins.
    let holders: Vec<Uuid> = bundle.publishers.iter().map(|p| p.holder_user_id).collect();
    let admins: HashSet<Uuid> = sqlx::query_scalar!(
        "SELECT id FROM users WHERE id = ANY($1) AND role IN ('admin', 'owner') AND disabled_at IS NULL",
        &holders
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    let unknown: Vec<FieldError> = bundle
        .publishers
        .iter()
        .enumerate()
        .filter(|(_, p)| !admins.contains(&p.holder_user_id))
        .map(|(i, _)| FieldError {
            field: format!("publishers[{i}].holder_user_id"),
            code: "unknown_holder".into(),
            message: Some("must be an active admin or owner of this server".into()),
        })
        .collect();
    if !unknown.is_empty() {
        return Err(ApiError::unprocessable(
            "unknown_holder",
            "A publisher key holder is not an admin of this server",
        )
        .with_errors(unknown));
    }

    sqlx::query!(
        "INSERT INTO trust_bundles (version, bundle, signature, expires_at, uploaded_by) VALUES ($1, $2, $3, $4, $5)",
        version,
        &bytes,
        &signature.as_bytes()[..],
        bundle.expires_at.map(to_offset).transpose()?,
        owner.user_id
    )
    .execute(&mut *tx)
    .await?;
    for p in &bundle.publishers {
        let revocation = bundle.revoked.iter().find(|r| r.key_id == p.key_id);
        sqlx::query!(
            r#"INSERT INTO publisher_keys (key_id, public_key, holder_user_id, label, not_before, not_after,
                                           trust_version, revoked_at, revocation_reason)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               ON CONFLICT (key_id) DO UPDATE SET
                 holder_user_id = EXCLUDED.holder_user_id, label = EXCLUDED.label,
                 not_before = EXCLUDED.not_before, not_after = EXCLUDED.not_after,
                 trust_version = EXCLUDED.trust_version, revoked_at = EXCLUDED.revoked_at,
                 revocation_reason = EXCLUDED.revocation_reason"#,
            p.key_id.to_string(),
            &p.public_key.as_bytes()[..],
            p.holder_user_id,
            p.label,
            to_offset(p.not_before)?,
            to_offset(p.not_after)?,
            version,
            revocation.map(|r| to_offset(r.revoked_at)).transpose()?,
            revocation.map(|r| r.reason.as_str())
        )
        .execute(&mut *tx)
        .await?;
    }
    // Keys revoked without being listed stay visible, marked revoked, under this version.
    for r in &bundle.revoked {
        sqlx::query!(
            "UPDATE publisher_keys SET revoked_at = $2, revocation_reason = $3, trust_version = $4
             WHERE key_id = $1 AND trust_version < $4",
            r.key_id.to_string(),
            to_offset(r.revoked_at)?,
            r.reason,
            version
        )
        .execute(&mut *tx)
        .await?;
    }
    audit::record(
        &mut tx,
        owner.user_id,
        &meta,
        "trust.bundle_upload",
        "trust_bundle",
        &version.to_string(),
        json!({
            "publishers": bundle.publishers.len(),
            "revoked": bundle.revoked.len(),
            "rotated": verified.rotated,
            "next_root": bundle.next_root.as_ref().map(|n| n.key_id.to_string()),
        }),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(version, rotated = verified.rotated, "trust bundle stored");
    Ok((
        StatusCode::CREATED,
        axum::Json(TrustBundleStored { version }),
    )
        .into_response())
}

/// Publisher keys from the current trust bundle
#[utoipa::path(
    get,
    path = "/v1/admin/trust/publisher-keys",
    tag = "admin-server",
    operation_id = "adminListPublisherKeys",
    responses((status = 200, description = "Keys", body = inline(PublisherKeyList)), Unauthorized, Forbidden)
)]
pub async fn list_publisher_keys(
    State(state): State<AppState>,
    _admin: RequireAdmin,
) -> ApiResult<axum::Json<PublisherKeyList>> {
    let Some(current) =
        sqlx::query!("SELECT version, expires_at FROM trust_bundles ORDER BY version DESC LIMIT 1")
            .fetch_optional(&state.db)
            .await?
    else {
        return Ok(axum::Json(PublisherKeyList {
            bundle_version: 0,
            bundle_expires_at: None,
            items: Vec::new(),
        }));
    };
    let rows = sqlx::query!(
        r#"SELECT key_id, public_key, holder_user_id, label, not_before, not_after, revoked_at, revocation_reason
           FROM publisher_keys WHERE trust_version = $1 ORDER BY label, key_id"#,
        current.version
    )
    .fetch_all(&state.db)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.holder_user_id).collect();
    let users = packages::users_public(&state, &ids).await?;
    let items = rows
        .into_iter()
        .filter_map(|r| {
            Some(PublisherKey {
                holder: users.get(&r.holder_user_id)?.clone(),
                key_id: r.key_id,
                public_key: STANDARD.encode(&r.public_key),
                label: r.label,
                not_before: r.not_before,
                not_after: r.not_after,
                revoked_at: r.revoked_at,
                revocation_reason: r.revocation_reason,
            })
        })
        .collect();
    Ok(axum::Json(PublisherKeyList {
        bundle_version: current.version,
        bundle_expires_at: current.expires_at,
        items,
    }))
}
