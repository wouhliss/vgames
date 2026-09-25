//! Finalize and re-sign versions (A1-T11, 02-package-format §6 step 6, 01-security §3.4).
//!
//! Every signature and manifest rule is `vgames_core::verify::verify_manifest` (Agent 5);
//! this module fetches the uploaded bytes, maps each failure to its own 422 code and
//! records the verified facts.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use bytes::BytesMut;
use futures_util::StreamExt;
use serde_json::json;
use time::OffsetDateTime;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_core::{
    Digest, Envelope, Timestamp,
    manifest::{Manifest, Platform},
    sign::SignatureError,
    verify::{ExpectedRelease, VerifiedManifest, VerifyError, VerifyMode, verify_manifest},
};
use vgames_proto::{
    FieldError,
    versions::{FinalizeRequest, ReplaceSignature, SignatureEnvelope, Version, VersionState},
};

use crate::{
    audit,
    auth::{RequestMeta, RequireAdmin},
    error::{ApiError, ApiResult},
    http::json::{Json, Validate, invalid},
    jobs::{self, Enqueue},
    openapi_problems::{BadRequest, Conflict, Forbidden, NotFound, Unauthorized, Unprocessable},
    state::AppState,
    storage::BucketKind,
    trust,
    versions::{self, MAX_MANIFEST_BYTES, manifest_object, pack_object, signature_object},
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(finalize))
        .routes(routes!(replace_signature))
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Validate for FinalizeRequest {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if !(1..=MAX_MANIFEST_BYTES as i64).contains(&self.manifest_size) {
            invalid(
                errors,
                "manifest_size",
                "out_of_range",
                "must be 1..=268435456",
            );
        }
        if !is_hex64(&self.manifest_blake3) {
            invalid(
                errors,
                "manifest_blake3",
                "pattern",
                "must be 64 lowercase hex characters",
            );
        }
    }
}

impl Validate for ReplaceSignature {
    fn validate(&self, _errors: &mut Vec<FieldError>) {}
}

fn unprocessable(code: &'static str, title: &'static str, detail: impl Into<String>) -> ApiError {
    ApiError::unprocessable(code, title).with_detail(detail.into())
}

/// Parses the wire envelope with `vgames-core`'s strict rules.
fn parse_envelope(env: &SignatureEnvelope) -> ApiResult<Envelope> {
    let bytes = serde_json::to_vec(env).map_err(ApiError::internal_from)?;
    Envelope::parse(&bytes).map_err(|e| {
        unprocessable(
            "signature_invalid",
            "The signature envelope is invalid",
            e.to_string(),
        )
    })
}

fn verify_error(e: VerifyError) -> ApiError {
    let detail = e.to_string();
    match e {
        VerifyError::UnknownKey(_)
        | VerifyError::RevokedKey(_)
        | VerifyError::TrustServerMismatch
        | VerifyError::TrustExpired => unprocessable(
            "publisher_key_untrusted",
            "The signing key is not trusted by this server",
            detail,
        ),
        VerifyError::NotKeyHolder(_) => unprocessable(
            "publisher_key_not_yours",
            "The signing key belongs to another admin",
            detail,
        ),
        VerifyError::KeyNotValidNow(_) => unprocessable(
            "publisher_key_expired",
            "The signing key is outside its validity window",
            detail,
        ),
        VerifyError::Signature(SignatureError::PayloadDigestMismatch) => unprocessable(
            "manifest_hash_mismatch",
            "The signature is over different manifest bytes",
            detail,
        ),
        VerifyError::Manifest(m) => {
            ApiError::unprocessable("manifest_invalid", "The manifest is invalid").with_errors(
                vec![FieldError {
                    field: "manifest".into(),
                    code: "invalid".into(),
                    message: Some(m.to_string()),
                }],
            )
        }
        VerifyError::Mismatch { field } => {
            ApiError::unprocessable("manifest_mismatch", "The manifest is for another version")
                .with_errors(vec![FieldError {
                    field: field.into(),
                    code: "mismatch".into(),
                    message: Some(detail),
                }])
        }
        _ => unprocessable("signature_invalid", "The signature does not verify", detail),
    }
}

/// Reads the uploaded manifest, refusing more than `expected_size` bytes.
async fn read_manifest(state: &AppState, name: &str, expected_size: u64) -> ApiResult<Vec<u8>> {
    let meta = state
        .storage
        .head(BucketKind::Packages, name)
        .await?
        .ok_or_else(|| {
            unprocessable(
                "manifest_missing",
                "manifest.json has not been uploaded",
                name,
            )
        })?;
    if meta.size != expected_size {
        return Err(unprocessable(
            "manifest_hash_mismatch",
            "The uploaded manifest does not match",
            format!("stored size {} != declared {expected_size}", meta.size),
        ));
    }
    let mut stream = state
        .storage
        .get_range_stream(BucketKind::Packages, name, None)
        .await?;
    let mut buf = BytesMut::with_capacity(usize::try_from(expected_size).unwrap_or(0));
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if (buf.len() + chunk.len()) as u64 > expected_size {
            return Err(unprocessable(
                "manifest_hash_mismatch",
                "The uploaded manifest does not match",
                "longer than declared",
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf.to_vec())
}

struct VersionRow {
    package_id: Uuid,
    platform: String,
    sequence: i64,
    state: String,
    created_by: Uuid,
}

async fn version_row(state: &AppState, id: Uuid) -> ApiResult<VersionRow> {
    sqlx::query_as!(
        VersionRow,
        r#"SELECT v.package_id, v.platform, v.sequence, v.state, v.created_by FROM package_versions v
           JOIN packages p ON p.id = v.package_id AND p.deleted_at IS NULL WHERE v.id = $1"#,
        id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)
}

/// Runs `verify_manifest` in server mode for `caller` against the current trust state.
async fn verify(
    state: &AppState,
    row: &VersionRow,
    version_id: Uuid,
    envelope: &Envelope,
    manifest: &[u8],
    caller: Uuid,
) -> ApiResult<VerifiedManifest> {
    let trust = trust::current_state(state).await?.ok_or_else(|| {
        unprocessable(
            "publisher_key_untrusted",
            "The signing key is not trusted by this server",
            "no trust bundle has been uploaded",
        )
    })?;
    let expected = ExpectedRelease {
        server_id: state.config.server_id,
        package_id: row.package_id,
        version_id,
        platform: row
            .platform
            .parse::<Platform>()
            .map_err(|()| ApiError::internal())?,
        sequence: u64::try_from(row.sequence).map_err(ApiError::internal_from)?,
    };
    let mode = VerifyMode::Server {
        now: Timestamp::from(OffsetDateTime::now_utc()),
        caller,
    };
    verify_manifest(&trust, envelope, manifest, &expected, None, mode).map_err(verify_error)
}

/// Every pack of the manifest must be uploaded with exactly the listed size.
async fn check_packs(
    state: &AppState,
    package_id: Uuid,
    version_id: Uuid,
    manifest: &Manifest,
) -> ApiResult<()> {
    let (mut missing, mut wrong) = (Vec::new(), Vec::new());
    for (i, pack) in manifest.packs.iter().enumerate() {
        let index = u32::try_from(i).map_err(ApiError::internal_from)?;
        match state
            .storage
            .head(
                BucketKind::Packages,
                &pack_object(package_id, version_id, index),
            )
            .await?
        {
            None => missing.push(FieldError {
                field: format!("packs[{i}]"),
                code: "missing".into(),
                message: None,
            }),
            Some(m) if m.size != pack.size => wrong.push(FieldError {
                field: format!("packs[{i}]"),
                code: "size_mismatch".into(),
                message: Some(format!(
                    "uploaded {} bytes, manifest says {}",
                    m.size, pack.size
                )),
            }),
            Some(_) => {}
        }
    }
    if !missing.is_empty() {
        return Err(
            ApiError::unprocessable("pack_missing", "Some packs have not been uploaded")
                .with_errors(missing),
        );
    }
    if !wrong.is_empty() {
        return Err(ApiError::unprocessable(
            "pack_size_mismatch",
            "Some packs do not have the manifest size",
        )
        .with_errors(wrong));
    }
    Ok(())
}

fn counts(m: &Manifest) -> ApiResult<(i32, i64, i32, i32)> {
    let too_big =
        |_| ApiError::unprocessable("manifest_invalid", "The manifest totals are out of range");
    Ok((
        i32::try_from(m.packs.len()).map_err(too_big)?,
        i64::try_from(m.totals.bytes).map_err(too_big)?,
        i32::try_from(m.files.len()).map_err(too_big)?,
        i32::try_from(m.chunks.len()).map_err(too_big)?,
    ))
}

/// Verify manifest + signature, then start server-side verification
#[utoipa::path(
    post,
    path = "/v1/admin/versions/{version_id}/finalize",
    tag = "admin-packages",
    operation_id = "adminFinalizeVersion",
    params(("version_id" = Uuid, Path)),
    request_body = FinalizeRequest,
    responses(
        (status = 202, description = "Accepted, state `verifying`", body = Version),
        BadRequest, Unauthorized, Forbidden, NotFound, Conflict, Unprocessable
    )
)]
pub async fn finalize(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Path(version_id): Path<Uuid>,
    Json(req): Json<FinalizeRequest>,
) -> ApiResult<Response> {
    let row = version_row(&state, version_id).await?;
    if row.created_by != admin.user_id {
        return Err(ApiError::forbidden()
            .with_detail("Only the admin who created this version can finalize it."));
    }
    if row.state != VersionState::Uploading.as_str() {
        return Err(ApiError::conflict(
            "version_not_uploading",
            "This version is already finalized or aborted",
        ));
    }
    let envelope = parse_envelope(&req.signature)?;
    if envelope.payload_blake3.to_hex() != req.manifest_blake3 {
        return Err(unprocessable(
            "manifest_hash_mismatch",
            "The signature is over different manifest bytes",
            "payload_blake3 != manifest_blake3",
        ));
    }
    let size = u64::try_from(req.manifest_size).map_err(ApiError::internal_from)?;
    let manifest_name = manifest_object(row.package_id, version_id);
    let bytes = read_manifest(&state, &manifest_name, size).await?;
    let digest = Digest::of(&bytes);
    if digest.to_hex() != req.manifest_blake3 {
        return Err(unprocessable(
            "manifest_hash_mismatch",
            "The uploaded manifest does not match",
            "BLAKE3 differs",
        ));
    }
    let verified = verify(&state, &row, version_id, &envelope, &bytes, admin.user_id).await?;
    check_packs(&state, row.package_id, version_id, &verified.manifest).await?;
    let (pack_count, total_size, file_count, chunk_count) = counts(&verified.manifest)?;

    state
        .storage
        .put_small(
            BucketKind::Packages,
            &signature_object(row.package_id, version_id),
            envelope.to_bytes().into(),
            "application/json",
        )
        .await?;
    let mut tx = state.db.begin().await?;
    let updated = sqlx::query!(
        r#"UPDATE package_versions SET state = 'verifying', manifest_object = $2, manifest_size = $3,
             manifest_blake3 = $4, signature = $5, publisher_key_id = $6, pack_count = $7, total_size = $8,
             file_count = $9, chunk_count = $10, finalized_at = now()
           WHERE id = $1 AND state = 'uploading'"#,
        version_id,
        manifest_name,
        req.manifest_size,
        verified.digest.as_bytes().as_slice(),
        envelope.signature.as_bytes().as_slice(),
        verified.key_id.to_string(),
        pack_count,
        total_size,
        file_count,
        chunk_count
    )
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "version_not_uploading",
            "This version is already finalized or aborted",
        ));
    }
    for (i, pack) in verified.manifest.packs.iter().enumerate() {
        let index = u32::try_from(i).map_err(ApiError::internal_from)?;
        sqlx::query!(
            r#"INSERT INTO package_packs (version_id, pack_index, object_name, size, blake3, uploaded_at)
               VALUES ($1, $2, $3, $4, $5, now())"#,
            version_id,
            i32::try_from(index).map_err(ApiError::internal_from)?,
            pack_object(row.package_id, version_id, index),
            i64::try_from(pack.size).map_err(ApiError::internal_from)?,
            pack.blake3.as_bytes().as_slice()
        )
        .execute(&mut *tx)
        .await?;
    }
    jobs::enqueue(
        &mut tx,
        "version.verify",
        json!({ "version_id": version_id.to_string() }),
        Enqueue {
            dedupe_key: Some(format!("version.verify:{version_id}")),
            ..Default::default()
        },
    )
    .await?;
    audit::record(
        &mut tx,
        admin.user_id,
        &meta,
        "version.finalize",
        "version",
        &version_id.to_string(),
        json!({ "key_id": verified.key_id.to_string(), "manifest_blake3": req.manifest_blake3, "packs": pack_count }),
    )
    .await?;
    tx.commit().await?;
    let version = versions::load_one(&state, version_id).await?;
    Ok((StatusCode::ACCEPTED, axum::Json(version)).into_response())
}

/// Replace a version's signature envelope (re-sign after key revocation or expiry)
#[utoipa::path(
    post,
    path = "/v1/admin/versions/{version_id}/signature",
    tag = "admin-packages",
    operation_id = "adminReplaceSignature",
    params(("version_id" = Uuid, Path)),
    request_body = ReplaceSignature,
    responses(
        (status = 200, description = "Signature replaced", body = Version),
        BadRequest, Unauthorized, Forbidden, NotFound, Conflict, Unprocessable
    )
)]
pub async fn replace_signature(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Path(version_id): Path<Uuid>,
    Json(req): Json<ReplaceSignature>,
) -> ApiResult<axum::Json<Version>> {
    let row = version_row(&state, version_id).await?;
    if matches!(
        VersionState::parse(&row.state),
        Some(VersionState::Uploading | VersionState::Aborted) | None
    ) {
        return Err(ApiError::conflict(
            "version_not_finalized",
            "Only finalized versions carry a signature",
        ));
    }
    let stored = sqlx::query!(
        r#"SELECT manifest_size AS "size!", manifest_blake3 AS "blake3!", publisher_key_id AS "key_id!"
           FROM package_versions WHERE id = $1"#,
        version_id
    )
    .fetch_one(&state.db)
    .await?;
    let envelope = parse_envelope(&req.signature)?;
    // Manifest bytes never change: the new envelope must be over the stored digest.
    if envelope.payload_blake3.as_bytes().as_slice() != stored.blake3.as_slice() {
        return Err(unprocessable(
            "manifest_hash_mismatch",
            "The signature is over different manifest bytes",
            "payload_blake3 differs from the stored manifest",
        ));
    }
    let size = u64::try_from(stored.size).map_err(ApiError::internal_from)?;
    let bytes = read_manifest(&state, &manifest_object(row.package_id, version_id), size).await?;
    let verified = verify(&state, &row, version_id, &envelope, &bytes, admin.user_id).await?;

    state
        .storage
        .put_small(
            BucketKind::Packages,
            &signature_object(row.package_id, version_id),
            envelope.to_bytes().into(),
            "application/json",
        )
        .await?;
    let mut tx = state.db.begin().await?;
    sqlx::query!(
        "UPDATE package_versions SET signature = $2, publisher_key_id = $3 WHERE id = $1",
        version_id,
        envelope.signature.as_bytes().as_slice(),
        verified.key_id.to_string()
    )
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        admin.user_id,
        &meta,
        "version.resign",
        "version",
        &version_id.to_string(),
        json!({ "old_key_id": stored.key_id, "key_id": verified.key_id.to_string() }),
    )
    .await?;
    tx.commit().await?;
    Ok(axum::Json(versions::load_one(&state, version_id).await?))
}
