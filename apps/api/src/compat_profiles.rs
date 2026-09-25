//! Signed compatibility profiles (A1-T12, docs/architecture/09-compatibility.md §4).
//! Verification is `vgames_core::verify::verify_compat_profile`; documents are stored verbatim.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;
use time::OffsetDateTime;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_core::{
    Digest, Timestamp,
    compat::Target,
    verify::{ExpectedCompat, VerifyError, VerifyMode, verify_compat_profile},
};
use vgames_proto::{
    FieldError,
    versions::{
        CompatProfileList, CompatStatus, CompatTarget, CompatUpload, SignatureContext,
        SignatureEnvelope, SignedCompatProfile,
    },
};

use crate::{
    audit,
    auth::{CurrentUser, RequestMeta, RequireAdmin},
    error::{ApiError, ApiResult},
    finalize::{parse_envelope, verify_error},
    http::json::{Json, Validate},
    openapi_problems::{BadRequest, Conflict, Forbidden, NotFound, Unauthorized, Unprocessable},
    state::AppState,
    trust,
};

/// Stored documents are at most 64 KiB (`package_compat_profiles.document`).
pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_profiles))
        .routes(routes!(put_profile))
}

fn parse_target(s: &str) -> Option<CompatTarget> {
    match s {
        "linux" => Some(CompatTarget::Linux),
        "macos" => Some(CompatTarget::Macos),
        _ => None,
    }
}

fn parse_status(s: &str) -> Option<CompatStatus> {
    serde_json::from_value(json!(s)).ok()
}

fn to_wire(
    target: &str,
    revision: i64,
    status: &str,
    document: &[u8],
    signature: &[u8],
    key_id: String,
    created_at: OffsetDateTime,
) -> Option<SignedCompatProfile> {
    Some(SignedCompatProfile {
        target: parse_target(target)?,
        revision,
        status: parse_status(status)?,
        document: STANDARD.encode(document),
        signature: SignatureEnvelope {
            format: "vgames.sig/1".into(),
            alg: "ed25519".into(),
            context: SignatureContext::Compat,
            key_id,
            payload_blake3: Digest::of(document).to_hex(),
            signature: STANDARD.encode(signature),
        },
        created_at,
    })
}

/// Latest signed compatibility profile per target (linux, macos)
#[utoipa::path(
    get,
    path = "/v1/packages/{package_id}/compat",
    tag = "catalog",
    operation_id = "getCompatProfiles",
    params(("package_id" = Uuid, Path)),
    responses((status = 200, description = "Profiles (possibly empty)", body = inline(CompatProfileList)), Unauthorized, NotFound)
)]
pub async fn list_profiles(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(package_id): Path<Uuid>,
) -> ApiResult<axum::Json<CompatProfileList>> {
    let status = sqlx::query_scalar!(
        "SELECT status FROM packages WHERE id = $1 AND deleted_at IS NULL",
        package_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)?;
    if status != "published" && !user.is_admin() {
        return Err(ApiError::not_found());
    }
    let rows = sqlx::query!(
        r#"SELECT DISTINCT ON (target) target, revision, status, document, signature, publisher_key_id, created_at
           FROM package_compat_profiles WHERE package_id = $1 ORDER BY target, revision DESC"#,
        package_id
    )
    .fetch_all(&state.db)
    .await?;
    let items = rows
        .into_iter()
        .filter_map(|r| {
            to_wire(
                &r.target,
                r.revision,
                &r.status,
                &r.document,
                &r.signature,
                r.publisher_key_id,
                r.created_at,
            )
        })
        .collect();
    Ok(axum::Json(CompatProfileList { items }))
}

impl Validate for CompatUpload {
    fn validate(&self, _errors: &mut Vec<FieldError>) {}
}

fn compat_error(e: VerifyError) -> ApiError {
    let detail = e.to_string();
    match e {
        VerifyError::CompatRollback { .. } => ApiError::conflict(
            "stale_revision",
            "A profile with this revision or a newer one exists",
        )
        .with_detail(detail),
        VerifyError::CompatMismatch { field } => ApiError::unprocessable(
            "compat_mismatch",
            "The profile is for another package, target or server",
        )
        .with_errors(vec![FieldError {
            field: field.into(),
            code: "mismatch".into(),
            message: Some(detail),
        }]),
        VerifyError::Compat(c) => {
            ApiError::unprocessable("compat_invalid", "The compatibility profile is invalid")
                .with_errors(vec![FieldError {
                    field: "document".into(),
                    code: "invalid".into(),
                    message: Some(c.to_string()),
                }])
        }
        other => verify_error(other),
    }
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Path)]
pub struct CompatPath {
    pub package_id: Uuid,
    #[param(inline)]
    pub target: CompatTarget,
}

/// Publish a new signed compatibility profile revision
#[utoipa::path(
    put,
    path = "/v1/admin/packages/{package_id}/compat/{target}",
    tag = "admin-packages",
    operation_id = "adminPutCompatProfile",
    params(CompatPath),
    request_body = CompatUpload,
    responses(
        (status = 201, description = "Stored", body = SignedCompatProfile),
        BadRequest, Unauthorized, Forbidden, NotFound, Conflict, Unprocessable
    )
)]
pub async fn put_profile(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Path(CompatPath { package_id, target }): Path<CompatPath>,
    Json(req): Json<CompatUpload>,
) -> ApiResult<Response> {
    if req.document.len() > MAX_DOCUMENT_BYTES.div_ceil(3) * 4 {
        return Err(ApiError::field("document", "too_large", "at most 64 KiB"));
    }
    let document = STANDARD
        .decode(req.document.as_bytes())
        .ok()
        .filter(|d| !d.is_empty() && d.len() <= MAX_DOCUMENT_BYTES)
        .ok_or_else(|| {
            ApiError::field(
                "document",
                "base64",
                "must be standard base64 of 1..=65536 bytes",
            )
        })?;
    let envelope = parse_envelope(&req.signature)?;
    let trust = trust::current_state(&state).await?.ok_or_else(|| {
        ApiError::unprocessable(
            "publisher_key_untrusted",
            "The signing key is not trusted by this server",
        )
        .with_detail("no trust bundle has been uploaded")
    })?;

    let mut tx = state.db.begin().await?;
    // The package row lock serializes revisions per package.
    sqlx::query_scalar!(
        r#"SELECT 1 AS "x!" FROM packages WHERE id = $1 AND deleted_at IS NULL FOR UPDATE"#,
        package_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let last = sqlx::query_scalar!(
        "SELECT max(revision) FROM package_compat_profiles WHERE package_id = $1 AND target = $2",
        package_id,
        target.as_str()
    )
    .fetch_one(&mut *tx)
    .await?
    .and_then(|r| u64::try_from(r).ok());
    let expected = ExpectedCompat {
        server_id: state.config.server_id,
        package_id,
        target: match target {
            CompatTarget::Linux => Target::Linux,
            CompatTarget::Macos => Target::Macos,
        },
    };
    let mode = VerifyMode::Server {
        now: Timestamp::from(OffsetDateTime::now_utc()),
        caller: admin.user_id,
    };
    let verified = verify_compat_profile(&trust, &envelope, &document, &expected, last, mode)
        .map_err(compat_error)?;
    let revision = i64::try_from(verified.profile.revision)
        .map_err(|_| ApiError::unprocessable("compat_invalid", "The revision is out of range"))?;
    let status = serde_json::to_value(verified.profile.status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(ApiError::internal)?;
    let row = sqlx::query!(
        r#"INSERT INTO package_compat_profiles (package_id, target, revision, document, signature, publisher_key_id, status, created_by)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING created_at"#,
        package_id,
        target.as_str(),
        revision,
        &document,
        envelope.signature.as_bytes().as_slice(),
        verified.key_id.to_string(),
        status,
        admin.user_id
    )
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        admin.user_id,
        &meta,
        "compat.publish",
        "package",
        &package_id.to_string(),
        json!({ "target": target.as_str(), "revision": revision, "status": status, "key_id": verified.key_id.to_string() }),
    )
    .await?;
    tx.commit().await?;
    let body = to_wire(
        target.as_str(),
        revision,
        &status,
        &document,
        envelope.signature.as_bytes(),
        verified.key_id.to_string(),
        row.created_at,
    )
    .ok_or_else(ApiError::internal)?;
    Ok((StatusCode::CREATED, axum::Json(body)).into_response())
}
