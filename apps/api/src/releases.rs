//! Server-side pack verification, publishing and yanking (A1-T12, 02-package-format §6 steps 7–8).

use axum::extract::State;
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_pack::verify::{PackExpectation, PackStreamVerifier, expectations_from_manifest};
use vgames_proto::{
    FieldError,
    versions::{Version, VersionState, YankRequest},
};

use crate::http::path::Path;
use crate::{
    audit,
    auth::{RequestMeta, RequireAdmin},
    error::{ApiError, ApiResult},
    finalize::read_manifest,
    http::json::{Json, Validate, invalid},
    jobs::{JobContext, JobError, JobHandler, handler},
    openapi_problems::{BadRequest, Conflict, Forbidden, NotFound, Unauthorized},
    realtime::{bus, hub::Target},
    state::AppState,
    storage::{BucketKind, StorageError},
    versions::{self, pack_object},
};

/// Packs streamed at once by `version.verify`.
const VERIFY_PARALLELISM: usize = 8;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(publish))
        .routes(routes!(yank))
}

pub fn job_handlers() -> Vec<(&'static str, JobHandler)> {
    vec![("version.verify", handler(verify_job))]
}

/// Tells every admin's sockets about a version state change (03-api §6 `version.state`).
pub(crate) async fn notify_admins(
    state: &AppState,
    version_id: Uuid,
    new_state: VersionState,
    failure_reason: Option<&str>,
) {
    let admins = match sqlx::query_scalar!(
        "SELECT id FROM users WHERE role IN ('admin', 'owner') AND disabled_at IS NULL"
    )
    .fetch_all(&state.db)
    .await
    {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "could not list admins for version.state");
            return;
        }
    };
    let data =
        json!({ "version_id": version_id, "state": new_state, "failure_reason": failure_reason });
    if let Err(e) = bus::publish(&state.db, &Target::users(&admins), "version.state", data).await {
        tracing::warn!(error = %e, "version.state not delivered");
    }
}

// ---------------------------------------------------------------------------------------
// version.verify
// ---------------------------------------------------------------------------------------

/// Why a pack is bad (content problems are final; storage trouble is retried).
pub(crate) enum PackOutcome {
    Ok,
    Bad(String),
}

/// Streams one pack through the verifier on the blocking pool.
pub(crate) async fn verify_pack(
    state: AppState,
    package_id: Uuid,
    version_id: Uuid,
    exp: PackExpectation,
) -> Result<PackOutcome, JobError> {
    let index = exp.index;
    let name = pack_object(package_id, version_id, index);
    let mut body = match state
        .storage
        .get_range_stream(BucketKind::Packages, &name, None)
        .await
    {
        Ok(s) => s,
        Err(StorageError::NotFound) => {
            return Ok(PackOutcome::Bad(format!(
                "pack {index}: missing from storage"
            )));
        }
        Err(e) => return Err(JobError::Retry(e.to_string())),
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Bytes>(4);
    let worker = tokio::task::spawn_blocking(move || {
        let mut verifier = PackStreamVerifier::new(exp);
        let mut first_bad: Option<String> = None;
        while let Some(piece) = rx.blocking_recv() {
            let pushed = verifier.push(&piece, |verdict| {
                if let (Err(e), None) = (&verdict.result, &first_bad) {
                    first_bad = Some(format!("pack {index}: chunk {}: {e}", verdict.index));
                }
            });
            if let Err(e) = pushed {
                return first_bad
                    .unwrap_or_else(|| format!("pack {index}: {e}"))
                    .into();
            }
            if first_bad.is_some() {
                return first_bad;
            }
        }
        match verifier.finish() {
            Ok(_) => None,
            Err(e) => Some(format!("pack {index}: {e}")),
        }
    });
    while let Some(piece) = body.next().await {
        let piece = piece.map_err(|e| JobError::Retry(e.to_string()))?;
        if tx.send(piece).await.is_err() {
            break; // the verifier stopped at a bad chunk
        }
    }
    drop(tx);
    Ok(match worker.await? {
        None => PackOutcome::Ok,
        Some(reason) => PackOutcome::Bad(reason),
    })
}

async fn verify_job(ctx: JobContext) -> Result<(), JobError> {
    let version_id = ctx
        .payload
        .get("version_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| JobError::Fatal("invalid payload".into()))?;
    let state = ctx.state.clone();
    let Some(row) = sqlx::query!(
        "SELECT package_id, state, manifest_object, manifest_size FROM package_versions WHERE id = $1",
        version_id
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(());
    };
    if row.state != VersionState::Verifying.as_str() {
        return Ok(()); // aborted meanwhile, or already verified
    }
    let (Some(manifest_name), Some(size)) = (row.manifest_object, row.manifest_size) else {
        return Err(JobError::Fatal(
            "verifying version without a manifest".into(),
        ));
    };
    let manifest = read_manifest(&state, &manifest_name, u64::try_from(size).unwrap_or(0))
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    let result = match expectations_from_manifest(&manifest) {
        Err(e) => Err(format!("manifest: {e}")),
        Ok(expectations) => {
            let total = expectations.len().max(1) as f32;
            let mut done = 0u32;
            let mut checks = stream::iter(expectations)
                .map(|exp| verify_pack(state.clone(), row.package_id, version_id, exp))
                .buffer_unordered(VERIFY_PARALLELISM);
            let mut outcome = Ok(());
            while let Some(pack) = checks.next().await {
                match pack? {
                    PackOutcome::Ok => {
                        done += 1;
                        sqlx::query!(
                            "UPDATE package_versions SET verify_progress = $2 WHERE id = $1 AND state = 'verifying'",
                            version_id,
                            (done as f32 / total).min(1.0)
                        )
                        .execute(&state.db)
                        .await?;
                    }
                    PackOutcome::Bad(reason) => {
                        outcome = Err(reason);
                        break;
                    }
                }
            }
            outcome
        }
    };
    let (new_state, reason) = match &result {
        Ok(()) => (VersionState::Ready, None),
        Err(r) => (
            VersionState::Failed,
            Some(r.chars().take(2000).collect::<String>()),
        ),
    };
    let updated = sqlx::query!(
        r#"UPDATE package_versions SET state = $2, failure_reason = $3, verified_at = now(),
             verify_progress = CASE WHEN $2 = 'ready' THEN 1 ELSE verify_progress END
           WHERE id = $1 AND state = 'verifying'"#,
        version_id,
        new_state.as_str(),
        reason
    )
    .execute(&state.db)
    .await?;
    if updated.rows_affected() == 1 {
        tracing::info!(%version_id, state = new_state.as_str(), reason = reason.as_deref(), "version verified");
        notify_admins(&state, version_id, new_state, reason.as_deref()).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Publish / yank
// ---------------------------------------------------------------------------------------

/// Make this version the current release for its platform
#[utoipa::path(
    post,
    path = "/v1/admin/versions/{version_id}/publish",
    tag = "admin-packages",
    operation_id = "adminPublishVersion",
    params(("version_id" = Uuid, Path)),
    responses((status = 200, description = "Published", body = Version), Unauthorized, Forbidden, NotFound, Conflict)
)]
pub async fn publish(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Path(version_id): Path<Uuid>,
) -> ApiResult<axum::Json<Version>> {
    let mut tx = state.db.begin().await?;
    let row = sqlx::query!(
        r#"SELECT v.package_id, v.platform, v.state FROM package_versions v
           JOIN packages p ON p.id = v.package_id AND p.deleted_at IS NULL
           WHERE v.id = $1 FOR UPDATE OF v"#,
        version_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    // Re-publishing an older published version is how admins roll back.
    if !matches!(
        VersionState::parse(&row.state),
        Some(VersionState::Ready | VersionState::Published)
    ) {
        return Err(ApiError::conflict(
            "version_not_ready",
            "Only verified versions can be published",
        )
        .with_detail(format!("the version is {}", row.state)));
    }
    sqlx::query!(
        "UPDATE package_versions SET state = 'published', published_at = COALESCE(published_at, now()) WHERE id = $1",
        version_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"INSERT INTO package_releases (package_id, platform, version_id, updated_by) VALUES ($1, $2, $3, $4)
           ON CONFLICT (package_id, platform) DO UPDATE SET
             version_id = EXCLUDED.version_id, updated_by = EXCLUDED.updated_by, updated_at = now()"#,
        row.package_id,
        row.platform,
        version_id,
        admin.user_id
    )
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        admin.user_id,
        &meta,
        "version.publish",
        "version",
        &version_id.to_string(),
        json!({ "package_id": row.package_id, "platform": row.platform }),
    )
    .await?;
    tx.commit().await?;
    notify_admins(&state, version_id, VersionState::Published, None).await;
    Ok(axum::Json(versions::load_one(&state, version_id).await?))
}

impl Validate for YankRequest {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if !(3..=500).contains(&self.reason.trim().chars().count()) {
            invalid(errors, "reason", "length", "must be 3-500 characters");
        }
    }
}

/// Withdraw a published version
#[utoipa::path(
    post,
    path = "/v1/admin/versions/{version_id}/yank",
    tag = "admin-packages",
    operation_id = "adminYankVersion",
    params(("version_id" = Uuid, Path)),
    request_body = YankRequest,
    responses((status = 200, description = "Yanked", body = Version), BadRequest, Unauthorized, Forbidden, NotFound, Conflict)
)]
pub async fn yank(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Path(version_id): Path<Uuid>,
    Json(req): Json<YankRequest>,
) -> ApiResult<axum::Json<Version>> {
    let mut tx = state.db.begin().await?;
    let row = sqlx::query!(
        "SELECT package_id, platform, state FROM package_versions WHERE id = $1 FOR UPDATE",
        version_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    if row.state != VersionState::Published.as_str() {
        return Err(ApiError::conflict(
            "version_not_published",
            "Only published versions can be yanked",
        )
        .with_detail(format!("the version is {}", row.state)));
    }
    let reason = req.reason.trim();
    sqlx::query!(
        "UPDATE package_versions SET state = 'yanked', yanked_at = now(), yank_reason = $2 WHERE id = $1",
        version_id,
        reason
    )
    .execute(&mut *tx)
    .await?;
    // If it was the current release, fall back to the newest remaining published version.
    let replacement = sqlx::query_scalar!(
        r#"SELECT id FROM package_versions
           WHERE package_id = $1 AND platform = $2 AND state = 'published' AND id <> $3
           ORDER BY sequence DESC LIMIT 1"#,
        row.package_id,
        row.platform,
        version_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    match replacement {
        Some(other) => {
            sqlx::query!(
                "UPDATE package_releases SET version_id = $2, updated_by = $3, updated_at = now() WHERE version_id = $1",
                version_id,
                other,
                admin.user_id
            )
            .execute(&mut *tx)
            .await?;
        }
        None => {
            sqlx::query!(
                "DELETE FROM package_releases WHERE version_id = $1",
                version_id
            )
            .execute(&mut *tx)
            .await?;
        }
    }
    audit::record(
        &mut tx,
        admin.user_id,
        &meta,
        "version.yank",
        "version",
        &version_id.to_string(),
        json!({ "package_id": row.package_id, "platform": row.platform, "reason": reason, "release_now": replacement }),
    )
    .await?;
    tx.commit().await?;
    notify_admins(&state, version_id, VersionState::Yanked, Some(reason)).await;
    Ok(axum::Json(versions::load_one(&state, version_id).await?))
}
