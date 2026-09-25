//! What players' launchers call to install games (A1-T12, 03-api §3): the release
//! descriptor, signed pack URLs and integrity reports.
//!
//! Signed URLs are bearer credentials: they are returned to the caller and never logged.

use axum::{
    extract::{Path, State},
    http::StatusCode,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_pack::verify::expectations_from_manifest;
use vgames_proto::{
    FieldError,
    packages::Platform,
    versions::{
        DownloadUrlsRequest, IntegrityReport, ManifestLink, PackUrl, PackUrlList,
        ReleaseDescriptor, SignatureContext, SignatureEnvelope, VersionState,
    },
};

use crate::{
    auth::CurrentUser,
    error::{ApiError, ApiResult},
    finalize::read_manifest,
    http::{
        json::{Json, Validate, invalid},
        ratelimit::Policy,
    },
    jobs::{self, Enqueue, JobContext, JobError, JobHandler, handler},
    openapi_problems::{BadRequest, Gone, NotFound, TooManyRequests, Unauthorized},
    releases::{PackOutcome, notify_admins, verify_pack},
    state::AppState,
    storage::BucketKind,
};

/// Distinct users who must report the same pack within 24 h before it is re-verified.
pub const REPORTS_BEFORE_REVERIFY: i64 = 3;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_release))
        .routes(routes!(download_urls))
        .routes(routes!(report_integrity))
}

pub fn job_handlers() -> Vec<(&'static str, JobHandler)> {
    vec![("pack.reverify", handler(reverify_job))]
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Current release for a platform
#[utoipa::path(
    get,
    path = "/v1/packages/{package_id}/releases/{platform}",
    tag = "catalog",
    operation_id = "getRelease",
    params(("package_id" = Uuid, Path), ("platform" = Platform, Path)),
    responses((status = 200, description = "Release descriptor", body = ReleaseDescriptor), Unauthorized, NotFound)
)]
pub async fn get_release(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((package_id, platform)): Path<(Uuid, Platform)>,
) -> ApiResult<axum::Json<ReleaseDescriptor>> {
    let row = sqlx::query!(
        r#"SELECT v.id, v.sequence, v.version_label, v.total_size AS "total_size!", v.pack_count AS "pack_count!",
                  v.manifest_object AS "manifest_object!", v.manifest_size AS "manifest_size!",
                  v.manifest_blake3 AS "manifest_blake3!", v.signature AS "signature!",
                  v.publisher_key_id AS "publisher_key_id!", v.published_at AS "published_at!", p.status
           FROM package_releases r
           JOIN package_versions v ON v.id = r.version_id
           JOIN packages p ON p.id = r.package_id AND p.deleted_at IS NULL
           WHERE r.package_id = $1 AND r.platform = $2 AND v.state = 'published'"#,
        package_id,
        platform.as_str()
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)?;
    // Players see releases of published packages only; admins can test before publishing the package.
    if row.status != "published" && !user.is_admin() {
        return Err(ApiError::not_found());
    }
    let yanked = sqlx::query_scalar!(
        "SELECT id FROM package_versions WHERE package_id = $1 AND platform = $2 AND state = 'yanked' ORDER BY sequence",
        package_id,
        platform.as_str()
    )
    .fetch_all(&state.db)
    .await?;
    let link = state
        .storage
        .sign_get(
            BucketKind::Packages,
            &row.manifest_object,
            state.config.signed_url_ttl,
        )
        .await?;
    Ok(axum::Json(ReleaseDescriptor {
        package_id,
        version_id: row.id,
        platform,
        sequence: row.sequence,
        version_label: row.version_label,
        total_size: row.total_size,
        pack_count: row.pack_count,
        manifest: ManifestLink {
            url: link.url,
            size: row.manifest_size,
            blake3: hex(&row.manifest_blake3),
            expires_at: link.expires_at,
        },
        signature: SignatureEnvelope {
            format: "vgames.sig/1".into(),
            alg: "ed25519".into(),
            context: SignatureContext::Manifest,
            key_id: row.publisher_key_id,
            payload_blake3: hex(&row.manifest_blake3),
            signature: STANDARD.encode(&row.signature),
        },
        yanked_version_ids: yanked,
        published_at: row.published_at,
    }))
}

impl Validate for DownloadUrlsRequest {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if !(1..=500).contains(&self.packs.len()) {
            invalid(errors, "packs", "count", "ask for 1-500 packs");
        }
        if self.packs.iter().any(|&p| p > 99_999) {
            invalid(
                errors,
                "packs",
                "out_of_range",
                "pack indexes are 0..=99999",
            );
        }
    }
}

/// Signed GET URLs for packs
#[utoipa::path(
    post,
    path = "/v1/versions/{version_id}/download-urls",
    tag = "catalog",
    operation_id = "createDownloadUrls",
    params(("version_id" = Uuid, Path)),
    request_body = DownloadUrlsRequest,
    responses(
        (status = 200, description = "One URL per requested pack", body = inline(PackUrlList)),
        BadRequest, Unauthorized, NotFound, Gone, TooManyRequests
    )
)]
pub async fn download_urls(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(version_id): Path<Uuid>,
    Json(req): Json<DownloadUrlsRequest>,
) -> ApiResult<axum::Json<PackUrlList>> {
    let row = sqlx::query!(
        r#"SELECT v.state, v.pack_count, p.status FROM package_versions v
           JOIN packages p ON p.id = v.package_id AND p.deleted_at IS NULL WHERE v.id = $1"#,
        version_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)?;
    match VersionState::parse(&row.state) {
        Some(VersionState::Published) if row.status == "published" || user.is_admin() => {}
        Some(VersionState::Yanked) => {
            return Err(ApiError::gone(
                "version_yanked",
                "This version was withdrawn; install the current release",
            ));
        }
        _ => return Err(ApiError::not_found()),
    }
    let pack_count = u32::try_from(row.pack_count.unwrap_or(0)).unwrap_or(0);
    if let Some(bad) = req.packs.iter().find(|&&p| p >= pack_count) {
        return Err(ApiError::field(
            "packs",
            "out_of_range",
            format!("pack {bad} does not exist (this version has {pack_count})"),
        ));
    }
    state.limits.check_n(
        Policy::DownloadUrls,
        &user.user_id.to_string(),
        u32::try_from(req.packs.len()).unwrap_or(u32::MAX),
    )?;
    let wanted: Vec<i32> = req
        .packs
        .iter()
        .filter_map(|&p| i32::try_from(p).ok())
        .collect();
    let packs = sqlx::query!(
        r#"SELECT pack_index, object_name, size AS "size!" FROM package_packs
           WHERE version_id = $1 AND pack_index = ANY($2) ORDER BY pack_index"#,
        version_id,
        &wanted
    )
    .fetch_all(&state.db)
    .await?;
    let mut items = Vec::with_capacity(packs.len());
    for p in packs {
        let signed = state
            .storage
            .sign_get(
                BucketKind::Packages,
                &p.object_name,
                state.config.signed_url_ttl,
            )
            .await?;
        items.push(PackUrl {
            pack_index: u32::try_from(p.pack_index).map_err(ApiError::internal_from)?,
            url: signed.url,
            size: p.size,
            expires_at: signed.expires_at,
        });
    }
    tracing::debug!(%version_id, packs = items.len(), "download URLs issued");
    Ok(axum::Json(PackUrlList { items }))
}

impl Validate for IntegrityReport {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if self
            .detail
            .as_ref()
            .is_some_and(|d| d.chars().count() > 1000)
        {
            invalid(
                errors,
                "detail",
                "max_length",
                "must be at most 1000 characters",
            );
        }
    }
}

/// Report a chunk hash mismatch seen twice
#[utoipa::path(
    post,
    path = "/v1/versions/{version_id}/integrity-reports",
    tag = "catalog",
    operation_id = "reportIntegrity",
    params(("version_id" = Uuid, Path)),
    request_body = IntegrityReport,
    responses((status = 202, description = "Recorded"), BadRequest, Unauthorized, NotFound, TooManyRequests)
)]
pub async fn report_integrity(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(version_id): Path<Uuid>,
    Json(req): Json<IntegrityReport>,
) -> ApiResult<StatusCode> {
    let pack_count = sqlx::query_scalar!(
        "SELECT pack_count FROM package_versions WHERE id = $1 AND state IN ('published', 'yanked')",
        version_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)?
    .unwrap_or(0);
    let pack = i32::try_from(req.pack_index).unwrap_or(i32::MAX);
    if pack >= pack_count {
        return Err(ApiError::field(
            "pack_index",
            "out_of_range",
            "no such pack in this version",
        ));
    }
    let mut tx = state.db.begin().await?;
    sqlx::query!(
        "INSERT INTO integrity_reports (version_id, pack_index, chunk_index, user_id, detail) VALUES ($1, $2, $3, $4, $5)",
        version_id,
        pack,
        req.chunk_index.and_then(|c| i32::try_from(c).ok()),
        user.user_id,
        req.detail.as_deref().map(str::trim).filter(|d| !d.is_empty())
    )
    .execute(&mut *tx)
    .await?;
    let reporters = sqlx::query_scalar!(
        r#"SELECT count(DISTINCT user_id) AS "n!" FROM integrity_reports
           WHERE version_id = $1 AND pack_index = $2 AND created_at > now() - interval '24 hours'"#,
        version_id,
        pack
    )
    .fetch_one(&mut *tx)
    .await?;
    if reporters >= REPORTS_BEFORE_REVERIFY {
        jobs::enqueue(
            &mut tx,
            "pack.reverify",
            json!({ "version_id": version_id.to_string(), "pack_index": req.pack_index, "reporters": reporters }),
            Enqueue { dedupe_key: Some(format!("pack.reverify:{version_id}:{pack}")), ..Default::default() },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::ACCEPTED)
}

/// Re-verifies one reported pack and tells admins the outcome.
async fn reverify_job(ctx: JobContext) -> Result<(), JobError> {
    let bad = || JobError::Fatal("invalid payload".into());
    let version_id = ctx
        .payload
        .get("version_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(bad)?;
    let index = ctx
        .payload
        .get("pack_index")
        .and_then(Value::as_u64)
        .and_then(|i| u32::try_from(i).ok())
        .ok_or_else(bad)?;
    let state = ctx.state.clone();
    let Some(row) = sqlx::query!(
        r#"SELECT package_id, state, manifest_object AS "manifest_object!", manifest_size AS "manifest_size!"
           FROM package_versions WHERE id = $1 AND manifest_object IS NOT NULL"#,
        version_id
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(());
    };
    let manifest = read_manifest(
        &state,
        &row.manifest_object,
        u64::try_from(row.manifest_size).unwrap_or(0),
    )
    .await
    .map_err(|e| JobError::Retry(e.to_string()))?;
    let expectation = expectations_from_manifest(&manifest)
        .map_err(|e| JobError::Fatal(e.to_string()))?
        .into_iter()
        .find(|e| e.index == index)
        .ok_or_else(|| JobError::Fatal(format!("pack {index} is not in the manifest")))?;
    let current = VersionState::parse(&row.state).unwrap_or(VersionState::Published);
    match verify_pack(state.clone(), row.package_id, version_id, expectation).await? {
        PackOutcome::Ok => {
            tracing::info!(%version_id, pack = index, "reported pack re-verified fine (client-side problem)");
        }
        PackOutcome::Bad(reason) => {
            tracing::error!(%version_id, pack = index, %reason, "reported pack is corrupt in storage");
            notify_admins(
                &state,
                version_id,
                current,
                Some(&format!("integrity: {reason}")),
            )
            .await;
        }
    }
    Ok(())
}
