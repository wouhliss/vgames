//! Package versions: creation, upload targets and abort (A1-T11, 02-package-format §6).
//! Finalize, verification and publishing build on these rows (A1-T11 part 2, A1-T12).

use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use utoipa::IntoParams;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_proto::{
    FieldError,
    packages::Platform,
    versions::{UploadMethod, UploadTarget, Version, VersionCreate, VersionPage, VersionState},
};

use crate::{
    audit,
    auth::{RequestMeta, RequireAdmin},
    error::{ApiError, ApiResult},
    http::{
        idempotency::{self, IdempotencyKey},
        json::{Json, Validate, invalid},
        pagination::{CursorCodec, PageParams, finish_page},
        query::Query,
    },
    jobs::{self, Enqueue, JobContext, JobError, JobHandler, handler},
    openapi_problems::{BadRequest, Conflict, Forbidden, NotFound, Unauthorized},
    packages,
    state::AppState,
    storage::{BucketKind, SignedRequest},
};

/// Start URLs for uploads are short-lived (01-security §5); sessions then last 7 days.
pub const UPLOAD_URL_TTL: Duration = Duration::from_secs(15 * 60);
pub const MAX_PACK_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_PACK_INDEX: u32 = 99_999;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_versions, create_version))
        .routes(routes!(get_version, abort_version))
        .routes(routes!(pack_upload_session))
        .routes(routes!(manifest_upload))
}

pub fn job_handlers() -> Vec<(&'static str, JobHandler)> {
    vec![("version.cleanup", handler(cleanup_job))]
}

// ---------------------------------------------------------------------------------------
// Object names (02-package-format §3)
// ---------------------------------------------------------------------------------------

pub fn version_prefix(package_id: Uuid, version_id: Uuid) -> String {
    format!("v1/{package_id}/{version_id}/")
}

pub fn pack_object(package_id: Uuid, version_id: Uuid, index: u32) -> String {
    format!("v1/{package_id}/{version_id}/packs/{index:05}.pack")
}

pub fn manifest_object(package_id: Uuid, version_id: Uuid) -> String {
    format!("v1/{package_id}/{version_id}/manifest.json")
}

pub fn signature_object(package_id: Uuid, version_id: Uuid) -> String {
    format!("v1/{package_id}/{version_id}/manifest.sig")
}

// ---------------------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------------------

/// Versions by id, newest first.
pub async fn load(state: &AppState, ids: &[Uuid]) -> ApiResult<Vec<Version>> {
    let rows = sqlx::query!(
        r#"SELECT v.id, v.package_id, v.platform, v.sequence, v.version_label, v.state, v.failure_reason,
                  v.total_size, v.file_count, v.chunk_count, v.pack_count, v.publisher_key_id,
                  v.created_at, v.created_by, v.finalized_at, v.verified_at, v.published_at, v.yanked_at,
                  EXISTS (SELECT 1 FROM package_releases r WHERE r.version_id = v.id) AS "is_current!"
           FROM package_versions v WHERE v.id = ANY($1) ORDER BY v.id DESC"#,
        ids
    )
    .fetch_all(&state.db)
    .await?;
    let creators: Vec<Uuid> = rows.iter().map(|r| r.created_by).collect();
    let users = packages::users_public(state, &creators).await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(Version {
                id: r.id,
                package_id: r.package_id,
                server_id: state.config.server_id,
                platform: Platform::parse(&r.platform)?,
                sequence: r.sequence,
                version_label: r.version_label,
                state: VersionState::parse(&r.state)?,
                is_current_release: Some(r.is_current),
                failure_reason: r.failure_reason,
                total_size: r.total_size,
                file_count: r.file_count,
                chunk_count: r.chunk_count,
                pack_count: r.pack_count,
                publisher_key_id: r.publisher_key_id,
                verify_progress: None,
                created_at: r.created_at,
                created_by: users.get(&r.created_by)?.clone(),
                finalized_at: r.finalized_at,
                verified_at: r.verified_at,
                published_at: r.published_at,
                yanked_at: r.yanked_at,
            })
        })
        .collect())
}

pub async fn load_one(state: &AppState, id: Uuid) -> ApiResult<Version> {
    load(state, &[id])
        .await?
        .into_iter()
        .next()
        .ok_or_else(ApiError::not_found)
}

async fn ensure_package(state: &AppState, id: Uuid) -> ApiResult<()> {
    sqlx::query_scalar!(
        r#"SELECT 1 AS "x!" FROM packages WHERE id = $1 AND deleted_at IS NULL"#,
        id
    )
    .fetch_optional(&state.db)
    .await?
    .map(|_| ())
    .ok_or_else(ApiError::not_found)
}

// ---------------------------------------------------------------------------------------
// Create / list / get
// ---------------------------------------------------------------------------------------

impl Validate for VersionCreate {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        let n = self.version_label.chars().count();
        if !(1..=64).contains(&n) || self.version_label.trim() != self.version_label {
            invalid(
                errors,
                "version_label",
                "length",
                "must be 1-64 characters without surrounding spaces",
            );
        }
        if self.version_label.chars().any(char::is_control) {
            invalid(
                errors,
                "version_label",
                "control_characters",
                "must not contain control characters",
            );
        }
    }
}

/// Start a version upload (assigns id and sequence)
#[utoipa::path(
    post,
    path = "/v1/admin/packages/{package_id}/versions",
    tag = "admin-packages",
    operation_id = "adminCreateVersion",
    params(
        ("package_id" = Uuid, Path),
        ("Idempotency-Key" = Option<String>, Header, pattern = "^[A-Za-z0-9_-]{16,128}$")
    ),
    request_body = VersionCreate,
    responses((status = 201, description = "Created in state `uploading`", body = Version), BadRequest, Unauthorized, Forbidden, NotFound)
)]
pub async fn create_version(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Path(package_id): Path<Uuid>,
    key: IdempotencyKey,
    Json(req): Json<VersionCreate>,
) -> ApiResult<Response> {
    let path = format!("/v1/admin/packages/{package_id}/versions");
    let fp = idempotency::fingerprint("POST", &path, &req);
    let st = state.clone();
    let resp = idempotency::run(&state.db, admin.user_id, &key, fp, || async move {
        let mut tx = st.db.begin().await?;
        // The package row lock serializes sequence allocation per package.
        sqlx::query_scalar!(
            r#"SELECT 1 AS "x!" FROM packages WHERE id = $1 AND deleted_at IS NULL FOR UPDATE"#,
            package_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(ApiError::not_found)?;
        let id = sqlx::query_scalar!(
            r#"INSERT INTO package_versions (package_id, platform, sequence, version_label, created_by)
               VALUES ($1, $2, (SELECT COALESCE(max(sequence), 0) + 1 FROM package_versions
                                WHERE package_id = $1 AND platform = $2), $3, $4)
               RETURNING id"#,
            package_id,
            req.platform.as_str(),
            req.version_label,
            admin.user_id
        )
        .fetch_one(&mut *tx)
        .await?;
        audit::record(
            &mut tx,
            admin.user_id,
            &meta,
            "version.create",
            "version",
            &id.to_string(),
            json!({ "package_id": package_id, "platform": req.platform, "version_label": req.version_label }),
        )
        .await?;
        tx.commit().await?;
        let version = load_one(&st, id).await?;
        Ok((StatusCode::CREATED, serde_json::to_value(version).map_err(ApiError::internal_from)?))
    })
    .await?;
    Ok(resp.into_response())
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct VersionListQuery {
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

impl Validate for VersionListQuery {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        PageParams {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
        .validate(errors);
    }
}

/// Versions of a package (all states)
#[utoipa::path(
    get,
    path = "/v1/admin/packages/{package_id}/versions",
    tag = "admin-packages",
    operation_id = "adminListVersions",
    params(("package_id" = Uuid, Path), VersionListQuery),
    responses((status = 200, description = "Page of versions (newest first)", body = VersionPage), Unauthorized, Forbidden, NotFound)
)]
pub async fn list_versions(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path(package_id): Path<Uuid>,
    Query(q): Query<VersionListQuery>,
) -> ApiResult<axum::Json<VersionPage>> {
    ensure_package(&state, package_id).await?;
    let limit = q.limit.unwrap_or(crate::http::pagination::DEFAULT_LIMIT);
    let filters = json!({ "package_id": package_id });
    let codec = CursorCodec::new(state.keys.cursor.expose());
    let after: Option<Uuid> = q
        .cursor
        .as_deref()
        .map(|c| codec.decode(c, &filters))
        .transpose()?
        .map(|(_, id): (String, Uuid)| id);
    // UUIDv7 ids sort by creation time.
    let ids = sqlx::query_scalar!(
        r#"SELECT id FROM package_versions WHERE package_id = $1 AND ($2::uuid IS NULL OR id < $2)
           ORDER BY id DESC LIMIT $3"#,
        package_id,
        after,
        i64::from(limit) + 1
    )
    .fetch_all(&state.db)
    .await?;
    let page = finish_page(ids, limit, |id| codec.encode(&String::new(), *id, &filters))?;
    let items = load(&state, &page.items).await?;
    Ok(axum::Json(VersionPage {
        items,
        next_cursor: page.next_cursor,
    }))
}

/// Version status
#[utoipa::path(
    get,
    path = "/v1/admin/versions/{version_id}",
    tag = "admin-packages",
    operation_id = "adminGetVersion",
    params(("version_id" = Uuid, Path)),
    responses((status = 200, description = "Version", body = Version), Unauthorized, Forbidden, NotFound)
)]
pub async fn get_version(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<axum::Json<Version>> {
    Ok(axum::Json(load_one(&state, id).await?))
}

// ---------------------------------------------------------------------------------------
// Upload targets
// ---------------------------------------------------------------------------------------

/// The package of a version the caller may upload to: its creator, while `uploading`.
async fn upload_scope(state: &AppState, version_id: Uuid, caller: Uuid) -> ApiResult<Uuid> {
    let row = sqlx::query!(
        r#"SELECT v.package_id, v.state, v.created_by FROM package_versions v
           JOIN packages p ON p.id = v.package_id AND p.deleted_at IS NULL
           WHERE v.id = $1"#,
        version_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)?;
    if row.created_by != caller {
        return Err(ApiError::forbidden()
            .with_detail("Only the admin who created this version can upload to it."));
    }
    if row.state != VersionState::Uploading.as_str() {
        return Err(ApiError::conflict(
            "version_not_uploading",
            "This version no longer accepts uploads",
        ));
    }
    Ok(row.package_id)
}

fn target(req: SignedRequest) -> UploadTarget {
    UploadTarget {
        url: req.url,
        method: if req.method == "PUT" {
            UploadMethod::Put
        } else {
            UploadMethod::Post
        },
        headers: req.headers.into_iter().collect(),
        expires_at: req.expires_at,
    }
}

/// Signed URL that starts a resumable upload for one pack
#[utoipa::path(
    post,
    path = "/v1/admin/versions/{version_id}/packs/{pack_index}/upload-session",
    tag = "admin-packages",
    operation_id = "adminCreatePackUploadSession",
    params(("version_id" = Uuid, Path), ("pack_index" = u32, Path, minimum = 0, maximum = 99999)),
    responses(
        (status = 200, description = "Start URL (POST with the returned headers; the Location response header is the session URI)", body = UploadTarget),
        Unauthorized, Forbidden, NotFound, Conflict
    )
)]
pub async fn pack_upload_session(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    Path((version_id, pack_index)): Path<(Uuid, u32)>,
) -> ApiResult<axum::Json<UploadTarget>> {
    if pack_index > MAX_PACK_INDEX {
        return Err(ApiError::field(
            "pack_index",
            "out_of_range",
            format!("must be 0..={MAX_PACK_INDEX}"),
        ));
    }
    let package_id = upload_scope(&state, version_id, admin.user_id).await?;
    let signed = state
        .storage
        .sign_resumable_start(
            BucketKind::Packages,
            &pack_object(package_id, version_id, pack_index),
            UPLOAD_URL_TTL,
            "application/octet-stream",
            1,
            MAX_PACK_BYTES,
        )
        .await?;
    Ok(axum::Json(target(signed)))
}

/// Signed PUT URL for manifest.json (≤ 256 MiB)
#[utoipa::path(
    post,
    path = "/v1/admin/versions/{version_id}/manifest-upload",
    tag = "admin-packages",
    operation_id = "adminCreateManifestUpload",
    params(("version_id" = Uuid, Path)),
    responses((status = 200, description = "Upload target", body = UploadTarget), Unauthorized, Forbidden, NotFound, Conflict)
)]
pub async fn manifest_upload(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    Path(version_id): Path<Uuid>,
) -> ApiResult<axum::Json<UploadTarget>> {
    let package_id = upload_scope(&state, version_id, admin.user_id).await?;
    let signed = state
        .storage
        .sign_put_range(
            BucketKind::Packages,
            &manifest_object(package_id, version_id),
            UPLOAD_URL_TTL,
            "application/json",
            1,
            MAX_MANIFEST_BYTES,
        )
        .await?;
    Ok(axum::Json(target(signed)))
}

// ---------------------------------------------------------------------------------------
// Abort
// ---------------------------------------------------------------------------------------

/// Abort an unpublished version (objects deleted by a job)
#[utoipa::path(
    delete,
    path = "/v1/admin/versions/{version_id}",
    tag = "admin-packages",
    operation_id = "adminAbortVersion",
    params(("version_id" = Uuid, Path)),
    responses((status = 204, description = "Aborted"), Unauthorized, Forbidden, NotFound, Conflict)
)]
pub async fn abort_version(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Path(version_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut tx = state.db.begin().await?;
    let row = sqlx::query!(
        "SELECT package_id, state FROM package_versions WHERE id = $1 FOR UPDATE",
        version_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    match VersionState::parse(&row.state) {
        Some(VersionState::Aborted) => return Ok(StatusCode::NO_CONTENT),
        Some(VersionState::Published | VersionState::Yanked) => {
            return Err(ApiError::conflict(
                "version_published",
                "Published versions are yanked, not aborted",
            ));
        }
        _ => {}
    }
    sqlx::query!(
        "UPDATE package_versions SET state = 'aborted' WHERE id = $1",
        version_id
    )
    .execute(&mut *tx)
    .await?;
    jobs::enqueue(
        &mut tx,
        "version.cleanup",
        json!({ "version_id": version_id.to_string() }),
        Enqueue {
            dedupe_key: Some(format!("version.cleanup:{version_id}")),
            ..Default::default()
        },
    )
    .await?;
    audit::record(
        &mut tx,
        admin.user_id,
        &meta,
        "version.abort",
        "version",
        &version_id.to_string(),
        json!({ "package_id": row.package_id, "from_state": row.state }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes every object of an aborted version.
async fn cleanup_job(ctx: JobContext) -> Result<(), JobError> {
    let version_id = ctx
        .payload
        .get("version_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| JobError::Fatal("invalid payload".into()))?;
    let Some(row) = sqlx::query!(
        "SELECT package_id, state FROM package_versions WHERE id = $1",
        version_id
    )
    .fetch_optional(&ctx.state.db)
    .await?
    else {
        return Ok(());
    };
    if row.state != VersionState::Aborted.as_str() {
        return Err(JobError::Fatal(format!(
            "version is {}, not aborted",
            row.state
        )));
    }
    let storage = &ctx.state.storage;
    let names = storage
        .list_prefix(
            BucketKind::Packages,
            &version_prefix(row.package_id, version_id),
        )
        .await?;
    for name in &names {
        storage.delete(BucketKind::Packages, name).await?;
    }
    sqlx::query!(
        "DELETE FROM package_packs WHERE version_id = $1",
        version_id
    )
    .execute(&ctx.state.db)
    .await?;
    tracing::info!(%version_id, objects = names.len(), "aborted version cleaned up");
    Ok(())
}
