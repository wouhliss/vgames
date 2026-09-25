//! Cloud saves (A1-T13, docs/architecture/06-cloud-saves.md §4).
//!
//! Blobs are content-addressed per user (`v1/{user}/{blake3}` in the saves bucket); a user only
//! ever sees their own saves. The head moves only by compare-and-swap on the parent snapshot.

use std::{collections::HashMap, time::Duration};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use utoipa::IntoParams;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_proto::{
    FieldError,
    saves::{
        BlobDownloadRequest, BlobUploadTarget, BlobUrl, BlobUrlList, CommitSnapshotRequest,
        OsFamily, PrepareBlobsRequest, PrepareBlobsResponse, SaveFile, SaveSnapshot,
        SaveSnapshotPage, SaveSnapshotSummary,
    },
};

use crate::{
    auth::CurrentUser,
    error::{ApiError, ApiResult},
    http::{
        idempotency::{self, IdempotencyKey},
        json::{Json, Validate, invalid},
        pagination::{CursorCodec, PageParams, finish_page},
        query::Query,
    },
    jobs::{JobContext, JobError},
    openapi_problems::{BadRequest, Conflict, NotFound, PayloadTooLarge, Unauthorized},
    state::AppState,
    storage::BucketKind,
};

pub const UPLOAD_URL_TTL: Duration = Duration::from_secs(15 * 60);
pub const MAX_FILES: usize = 10_000;
pub const MAX_BLOB_SIZE: i64 = 4 * 1024 * 1024 * 1024;
/// Snapshots kept per (user, package) besides the head.
pub const RETAINED_SNAPSHOTS: i64 = 20;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_head))
        .routes(routes!(list_snapshots, commit_snapshot))
        .routes(routes!(get_snapshot))
        .routes(routes!(prepare_blobs))
        .routes(routes!(blob_download_urls))
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    is_hex64(s).then(|| {
        (0..32)
            .filter_map(|i| u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok())
            .collect()
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn blob_object(user_id: Uuid, blake3_hex: &str) -> String {
    format!("v1/{user_id}/{blake3_hex}")
}

fn quota_exceeded(used: i64, quota: u64) -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "save_quota_exceeded",
        "The cloud save quota for this game is used up",
    )
    .with_detail(format!("{used} bytes needed, quota {quota}"))
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

/// Bytes of distinct blobs referenced by this user's retained snapshots of a package.
async fn used_bytes(conn: &mut sqlx::PgConnection, user: Uuid, package: Uuid) -> ApiResult<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT COALESCE(sum(b.size), 0)::bigint AS "n!" FROM save_blobs b
           WHERE b.user_id = $1 AND EXISTS (
             SELECT 1 FROM save_snapshot_blobs sb JOIN save_snapshots s ON s.id = sb.snapshot_id
             WHERE sb.user_id = b.user_id AND sb.blake3 = b.blake3 AND s.package_id = $2)"#,
        user,
        package
    )
    .fetch_one(conn)
    .await?)
}

// ---------------------------------------------------------------------------------------
// Reading snapshots
// ---------------------------------------------------------------------------------------

struct SnapshotRow {
    id: Uuid,
    package_id: Uuid,
    parent_id: Option<Uuid>,
    device_id: Option<Uuid>,
    device_name: Option<String>,
    platform: String,
    file_count: i32,
    total_size: i64,
    label: Option<String>,
    created_at: time::OffsetDateTime,
    files: serde_json::Value,
}

fn summary(r: &SnapshotRow) -> ApiResult<SaveSnapshotSummary> {
    Ok(SaveSnapshotSummary {
        id: r.id,
        package_id: r.package_id,
        parent_id: r.parent_id,
        device_id: r.device_id,
        device_name: r.device_name.clone(),
        platform: OsFamily::parse(&r.platform).ok_or_else(ApiError::internal)?,
        file_count: r.file_count,
        total_size: r.total_size,
        label: r.label.clone(),
        created_at: r.created_at,
    })
}

fn full(r: SnapshotRow) -> ApiResult<SaveSnapshot> {
    let summary = summary(&r)?;
    let files: Vec<SaveFile> = serde_json::from_value(r.files).map_err(ApiError::internal_from)?;
    Ok(SaveSnapshot { summary, files })
}

async fn load_snapshot(
    state: &AppState,
    user: Uuid,
    package: Uuid,
    id: Uuid,
) -> ApiResult<SnapshotRow> {
    sqlx::query_as!(
        SnapshotRow,
        r#"SELECT s.id, s.package_id, s.parent_id, s.device_id, d.display_name AS "device_name?", s.platform,
                  s.file_count, s.total_size, s.label, s.created_at, s.files
           FROM save_snapshots s LEFT JOIN devices d ON d.id = s.device_id
           WHERE s.id = $1 AND s.user_id = $2 AND s.package_id = $3"#,
        id,
        user,
        package
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)
}

/// Current cloud save head
#[utoipa::path(
    get,
    path = "/v1/saves/{package_id}/head",
    tag = "saves",
    operation_id = "getSaveHead",
    params(("package_id" = Uuid, Path)),
    responses((status = 200, description = "Head snapshot (with files)", body = SaveSnapshot), Unauthorized, NotFound)
)]
pub async fn get_head(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(package_id): Path<Uuid>,
) -> ApiResult<axum::Json<SaveSnapshot>> {
    let head = sqlx::query_scalar!(
        "SELECT snapshot_id FROM save_heads WHERE user_id = $1 AND package_id = $2",
        user.user_id,
        package_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok(axum::Json(full(
        load_snapshot(&state, user.user_id, package_id, head).await?,
    )?))
}

/// One snapshot with its file list
#[utoipa::path(
    get,
    path = "/v1/saves/{package_id}/snapshots/{snapshot_id}",
    tag = "saves",
    operation_id = "getSaveSnapshot",
    params(("package_id" = Uuid, Path), ("snapshot_id" = Uuid, Path)),
    responses((status = 200, description = "Snapshot", body = SaveSnapshot), Unauthorized, NotFound)
)]
pub async fn get_snapshot(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((package_id, snapshot_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<axum::Json<SaveSnapshot>> {
    Ok(axum::Json(full(
        load_snapshot(&state, user.user_id, package_id, snapshot_id).await?,
    )?))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct SnapshotListQuery {
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

impl Validate for SnapshotListQuery {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        PageParams {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
        .validate(errors);
    }
}

/// Snapshot history (newest first)
#[utoipa::path(
    get,
    path = "/v1/saves/{package_id}/snapshots",
    tag = "saves",
    operation_id = "listSaveSnapshots",
    params(("package_id" = Uuid, Path), SnapshotListQuery),
    responses((status = 200, description = "Page of snapshots (without file lists)", body = SaveSnapshotPage), Unauthorized)
)]
pub async fn list_snapshots(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(package_id): Path<Uuid>,
    Query(q): Query<SnapshotListQuery>,
) -> ApiResult<axum::Json<SaveSnapshotPage>> {
    let limit = q.limit.unwrap_or(crate::http::pagination::DEFAULT_LIMIT);
    let filters = json!({ "package_id": package_id, "user": user.user_id });
    let codec = CursorCodec::new(state.keys.cursor.expose());
    let after: Option<Uuid> = q
        .cursor
        .as_deref()
        .map(|c| codec.decode(c, &filters))
        .transpose()?
        .map(|(_, id): (String, Uuid)| id);
    let rows = sqlx::query_as!(
        SnapshotRow,
        r#"SELECT s.id, s.package_id, s.parent_id, s.device_id, d.display_name AS "device_name?", s.platform,
                  s.file_count, s.total_size, s.label, s.created_at, '[]'::jsonb AS "files!"
           FROM save_snapshots s LEFT JOIN devices d ON d.id = s.device_id
           WHERE s.user_id = $1 AND s.package_id = $2 AND ($3::uuid IS NULL OR s.id < $3)
           ORDER BY s.id DESC LIMIT $4"#,
        user.user_id,
        package_id,
        after,
        i64::from(limit) + 1
    )
    .fetch_all(&state.db)
    .await?;
    let page = finish_page(rows, limit, |r| {
        codec.encode(&String::new(), r.id, &filters)
    })?;
    let items = page
        .items
        .iter()
        .map(summary)
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(axum::Json(SaveSnapshotPage {
        items,
        next_cursor: page.next_cursor,
    }))
}

// ---------------------------------------------------------------------------------------
// Blobs
// ---------------------------------------------------------------------------------------

impl Validate for PrepareBlobsRequest {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if self.blobs.len() > MAX_FILES {
            invalid(errors, "blobs", "max_items", "at most 10000 blobs");
        }
        for (i, b) in self.blobs.iter().enumerate() {
            if !is_hex64(&b.blake3) {
                invalid(
                    errors,
                    &format!("blobs[{i}].blake3"),
                    "pattern",
                    "must be 64 lowercase hex characters",
                );
            }
            if !(0..=MAX_BLOB_SIZE).contains(&b.size) {
                invalid(
                    errors,
                    &format!("blobs[{i}].size"),
                    "out_of_range",
                    "must be 0..=4294967296",
                );
            }
        }
    }
}

/// Find missing blobs and get upload URLs
#[utoipa::path(
    post,
    path = "/v1/saves/{package_id}/blobs/prepare",
    tag = "saves",
    operation_id = "prepareSaveBlobs",
    params(("package_id" = Uuid, Path)),
    request_body = PrepareBlobsRequest,
    responses(
        (status = 200, description = "Upload targets for blobs the server does not have", body = inline(PrepareBlobsResponse)),
        BadRequest, Unauthorized, PayloadTooLarge
    )
)]
pub async fn prepare_blobs(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(package_id): Path<Uuid>,
    Json(req): Json<PrepareBlobsRequest>,
) -> ApiResult<axum::Json<PrepareBlobsResponse>> {
    ensure_package(&state, package_id).await?;
    let mut wanted: HashMap<String, i64> = HashMap::new();
    for b in &req.blobs {
        if let Some(prev) = wanted.insert(b.blake3.clone(), b.size)
            && prev != b.size
        {
            return Err(ApiError::field(
                "blobs",
                "size_mismatch",
                format!("{} is listed with two sizes", b.blake3),
            ));
        }
    }
    let hashes: Vec<Vec<u8>> = wanted.keys().filter_map(|h| decode_hex(h)).collect();
    let mut tx = state.db.begin().await?;
    let known = sqlx::query!(
        "SELECT blake3, size, uploaded_at IS NOT NULL AS \"uploaded!\" FROM save_blobs WHERE user_id = $1 AND blake3 = ANY($2)",
        user.user_id,
        &hashes
    )
    .fetch_all(&mut *tx)
    .await?;
    let known: HashMap<String, (i64, bool)> = known
        .into_iter()
        .map(|r| (hex(&r.blake3), (r.size, r.uploaded)))
        .collect();
    for (h, size) in &wanted {
        if let Some((known_size, _)) = known.get(h)
            && known_size != size
        {
            return Err(ApiError::field(
                "blobs",
                "size_mismatch",
                format!("{h} is known with another size"),
            ));
        }
    }
    // Quota: what this package's snapshots already use plus the blobs it does not reference yet.
    let used = used_bytes(&mut tx, user.user_id, package_id).await?;
    let referenced: std::collections::HashSet<String> = sqlx::query_scalar!(
        r#"SELECT DISTINCT sb.blake3 FROM save_snapshot_blobs sb JOIN save_snapshots s ON s.id = sb.snapshot_id
           WHERE s.user_id = $1 AND s.package_id = $2 AND sb.blake3 = ANY($3)"#,
        user.user_id,
        package_id,
        &hashes
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|b| hex(&b))
    .collect();
    let added: i64 = wanted
        .iter()
        .filter(|(h, _)| !referenced.contains(*h))
        .map(|(_, s)| *s)
        .sum();
    let quota = state.config.save_quota_bytes_per_package;
    if u64::try_from(used + added).unwrap_or(u64::MAX) > quota {
        return Err(quota_exceeded(used + added, quota));
    }

    let mut missing = Vec::new();
    for (h, size) in &wanted {
        if known.get(h).is_some_and(|(_, uploaded)| *uploaded) {
            continue;
        }
        let object = blob_object(user.user_id, h);
        sqlx::query!(
            "INSERT INTO save_blobs (user_id, blake3, size, object_name) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
            user.user_id,
            decode_hex(h).unwrap_or_default(),
            size,
            object
        )
        .execute(&mut *tx)
        .await?;
        let signed = state
            .storage
            .sign_put(
                BucketKind::Saves,
                &object,
                UPLOAD_URL_TTL,
                "application/octet-stream",
                u64::try_from(*size).unwrap_or(0),
            )
            .await?;
        missing.push(BlobUploadTarget {
            blake3: h.clone(),
            url: signed.url,
            method: "PUT".into(),
            headers: signed.headers.into_iter().collect(),
            expires_at: signed.expires_at,
        });
    }
    tx.commit().await?;
    missing.sort_by(|a, b| a.blake3.cmp(&b.blake3));
    Ok(axum::Json(PrepareBlobsResponse { missing }))
}

impl Validate for BlobDownloadRequest {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if !(1..=1000).contains(&self.blake3.len()) {
            invalid(errors, "blake3", "count", "ask for 1-1000 blobs");
        }
        if !self.blake3.iter().all(|h| is_hex64(h)) {
            invalid(
                errors,
                "blake3",
                "pattern",
                "must be 64 lowercase hex characters",
            );
        }
    }
}

/// Signed GET URLs for blobs
#[utoipa::path(
    post,
    path = "/v1/saves/{package_id}/blobs/download-urls",
    tag = "saves",
    operation_id = "createSaveBlobDownloadUrls",
    params(("package_id" = Uuid, Path)),
    request_body = BlobDownloadRequest,
    responses((status = 200, description = "URLs", body = inline(BlobUrlList)), BadRequest, Unauthorized, NotFound)
)]
pub async fn blob_download_urls(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(package_id): Path<Uuid>,
    Json(req): Json<BlobDownloadRequest>,
) -> ApiResult<axum::Json<BlobUrlList>> {
    let hashes: Vec<Vec<u8>> = req.blake3.iter().filter_map(|h| decode_hex(h)).collect();
    // Only uploaded blobs this user's snapshots of this package reference.
    let rows = sqlx::query!(
        r#"SELECT DISTINCT b.blake3, b.object_name FROM save_blobs b
           JOIN save_snapshot_blobs sb ON sb.user_id = b.user_id AND sb.blake3 = b.blake3
           JOIN save_snapshots s ON s.id = sb.snapshot_id AND s.package_id = $2
           WHERE b.user_id = $1 AND b.blake3 = ANY($3) AND b.uploaded_at IS NOT NULL"#,
        user.user_id,
        package_id,
        &hashes
    )
    .fetch_all(&state.db)
    .await?;
    if rows.len() != hashes.len() {
        return Err(ApiError::not_found()
            .with_detail("Some blobs are not part of your saves for this game."));
    }
    let mut items = Vec::with_capacity(rows.len());
    for r in rows {
        let signed = state
            .storage
            .sign_get(
                BucketKind::Saves,
                &r.object_name,
                state.config.signed_url_ttl,
            )
            .await?;
        items.push(BlobUrl {
            blake3: hex(&r.blake3),
            url: signed.url,
            expires_at: signed.expires_at,
        });
    }
    items.sort_by(|a, b| a.blake3.cmp(&b.blake3));
    Ok(axum::Json(BlobUrlList { items }))
}

// ---------------------------------------------------------------------------------------
// Commit
// ---------------------------------------------------------------------------------------

fn valid_save_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 512
        && !p.starts_with('/')
        && !p.contains('\\')
        && !p.contains('\0')
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

impl Validate for CommitSnapshotRequest {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if self.files.len() > MAX_FILES {
            invalid(errors, "files", "max_items", "at most 10000 files");
        }
        if self.label.as_ref().is_some_and(|l| l.chars().count() > 100) {
            invalid(errors, "label", "max_length", "at most 100 characters");
        }
        let mut seen = std::collections::HashSet::new();
        for (i, f) in self.files.iter().enumerate() {
            let at = |field: &str| format!("files[{i}].{field}");
            if !(1..=32).contains(&f.root.len())
                || !f
                    .root
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
            {
                invalid(
                    errors,
                    &at("root"),
                    "pattern",
                    "must match ^[a-z0-9_-]{1,32}$",
                );
            }
            if !valid_save_path(&f.path) {
                invalid(
                    errors,
                    &at("path"),
                    "invalid",
                    "must be a relative path without . or .. components",
                );
            }
            if !is_hex64(&f.blake3) {
                invalid(
                    errors,
                    &at("blake3"),
                    "pattern",
                    "must be 64 lowercase hex characters",
                );
            }
            if !(0..=MAX_BLOB_SIZE).contains(&f.size) {
                invalid(
                    errors,
                    &at("size"),
                    "out_of_range",
                    "must be 0..=4294967296",
                );
            }
            if !seen.insert((f.root.as_str(), f.path.as_str())) {
                invalid(errors, &at("path"), "duplicate", "listed twice");
            }
        }
    }
}

/// Commit a snapshot (compare-and-swap on parent)
#[utoipa::path(
    post,
    path = "/v1/saves/{package_id}/snapshots",
    tag = "saves",
    operation_id = "commitSaveSnapshot",
    params(
        ("package_id" = Uuid, Path),
        ("Idempotency-Key" = Option<String>, Header, pattern = "^[A-Za-z0-9_-]{16,128}$")
    ),
    request_body = CommitSnapshotRequest,
    responses(
        (status = 201, description = "New head", body = SaveSnapshot),
        BadRequest, Unauthorized, Conflict, PayloadTooLarge
    )
)]
pub async fn commit_snapshot(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(package_id): Path<Uuid>,
    key: IdempotencyKey,
    Json(req): Json<CommitSnapshotRequest>,
) -> ApiResult<Response> {
    ensure_package(&state, package_id).await?;
    let path = format!("/v1/saves/{package_id}/snapshots");
    let fp = idempotency::fingerprint("POST", &path, &req);
    let st = state.clone();
    let resp = idempotency::run(&state.db, user.user_id, &key, fp, || async move {
        let snapshot = commit(&st, &user, package_id, req).await?;
        Ok((
            StatusCode::CREATED,
            serde_json::to_value(snapshot).map_err(ApiError::internal_from)?,
        ))
    })
    .await?;
    Ok(resp.into_response())
}

async fn commit(
    state: &AppState,
    user: &CurrentUser,
    package_id: Uuid,
    req: CommitSnapshotRequest,
) -> ApiResult<SaveSnapshot> {
    // Distinct blobs and their sizes; one hash with two sizes is a client bug.
    let mut blobs: HashMap<String, i64> = HashMap::new();
    for f in &req.files {
        if let Some(prev) = blobs.insert(f.blake3.clone(), f.size)
            && prev != f.size
        {
            return Err(ApiError::field(
                "files",
                "size_mismatch",
                format!("{} is listed with two sizes", f.blake3),
            ));
        }
    }
    let hashes: Vec<Vec<u8>> = blobs.keys().filter_map(|h| decode_hex(h)).collect();
    let total_size: i64 = req.files.iter().map(|f| f.size).sum();

    let mut tx = state.db.begin().await?;
    let rows = sqlx::query!(
        r#"SELECT blake3, size, object_name, uploaded_at IS NOT NULL AS "uploaded!" FROM save_blobs
           WHERE user_id = $1 AND blake3 = ANY($2) FOR UPDATE"#,
        user.user_id,
        &hashes
    )
    .fetch_all(&mut *tx)
    .await?;
    let rows: HashMap<String, _> = rows.into_iter().map(|r| (hex(&r.blake3), r)).collect();
    let mut not_uploaded = Vec::new();
    for (h, size) in &blobs {
        let ok = match rows.get(h) {
            None => false,
            Some(r) if r.size != *size => false,
            Some(r) if r.uploaded => true,
            // First reference: confirm the upload through object metadata, size included.
            Some(r) => match state
                .storage
                .head(BucketKind::Saves, &r.object_name)
                .await?
            {
                Some(meta) if i64::try_from(meta.size).ok() == Some(r.size) => {
                    sqlx::query!(
                        "UPDATE save_blobs SET uploaded_at = now() WHERE user_id = $1 AND blake3 = $2",
                        user.user_id,
                        &r.blake3
                    )
                    .execute(&mut *tx)
                    .await?;
                    true
                }
                _ => false,
            },
        };
        if !ok {
            not_uploaded.push(FieldError {
                field: "files".into(),
                code: "blob_not_uploaded".into(),
                message: Some(h.clone()),
            });
        }
    }
    if !not_uploaded.is_empty() {
        return Err(
            ApiError::conflict("blob_not_uploaded", "Some files were not uploaded")
                .with_errors(not_uploaded),
        );
    }

    // Quota over what the package will reference once this snapshot exists.
    let used = used_bytes(&mut tx, user.user_id, package_id).await?;
    let referenced: std::collections::HashSet<String> = sqlx::query_scalar!(
        r#"SELECT DISTINCT sb.blake3 FROM save_snapshot_blobs sb JOIN save_snapshots s ON s.id = sb.snapshot_id
           WHERE s.user_id = $1 AND s.package_id = $2 AND sb.blake3 = ANY($3)"#,
        user.user_id,
        package_id,
        &hashes
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|b| hex(&b))
    .collect();
    let added: i64 = blobs
        .iter()
        .filter(|(h, _)| !referenced.contains(*h))
        .map(|(_, s)| *s)
        .sum();
    let quota = state.config.save_quota_bytes_per_package;
    if u64::try_from(used + added).unwrap_or(u64::MAX) > quota {
        return Err(quota_exceeded(used + added, quota));
    }

    let file_count = i32::try_from(req.files.len()).map_err(ApiError::internal_from)?;
    let files = serde_json::to_value(&req.files).map_err(ApiError::internal_from)?;
    let id = sqlx::query_scalar!(
        r#"INSERT INTO save_snapshots (user_id, package_id, parent_id, device_id, platform, file_count, total_size, files, label)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id"#,
        user.user_id,
        package_id,
        req.parent_snapshot_id,
        user.device_id,
        req.platform.as_str(),
        file_count,
        total_size,
        files,
        req.label.as_deref().map(str::trim).filter(|l| !l.is_empty())
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO save_snapshot_blobs (snapshot_id, user_id, blake3) SELECT $1, $2, unnest($3::bytea[])",
        id,
        user.user_id,
        &hashes
    )
    .execute(&mut *tx)
    .await?;
    // Compare-and-swap: the head moves only if it is still the parent this snapshot was based on.
    let moved = match req.parent_snapshot_id {
        Some(parent) => sqlx::query!(
            "UPDATE save_heads SET snapshot_id = $3, updated_at = now() WHERE user_id = $1 AND package_id = $2 AND snapshot_id = $4",
            user.user_id,
            package_id,
            id,
            parent
        )
        .execute(&mut *tx)
        .await?
        .rows_affected(),
        None => sqlx::query!(
            "INSERT INTO save_heads (user_id, package_id, snapshot_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            user.user_id,
            package_id,
            id
        )
        .execute(&mut *tx)
        .await?
        .rows_affected(),
    };
    if moved == 0 {
        drop(tx); // rollback
        let head = sqlx::query_scalar!(
            "SELECT snapshot_id FROM save_heads WHERE user_id = $1 AND package_id = $2",
            user.user_id,
            package_id
        )
        .fetch_optional(&state.db)
        .await?;
        return Err(ApiError::conflict(
            "save_head_conflict",
            "Your cloud saves changed on another device",
        )
        .with_detail(head.map_or_else(|| "none".to_string(), |h| h.to_string())));
    }
    tx.commit().await?;
    full(load_snapshot(state, user.user_id, package_id, id).await?)
}

// ---------------------------------------------------------------------------------------
// Retention and garbage collection (`saves.gc`, daily)
// ---------------------------------------------------------------------------------------

/// Keeps the head and the latest [`RETAINED_SNAPSHOTS`] snapshots per (user, package), then
/// deletes blobs no snapshot references (older than a day, so in-flight uploads survive).
pub async fn gc(ctx: JobContext) -> Result<(), JobError> {
    let state = &ctx.state;
    let dropped = sqlx::query!(
        r#"DELETE FROM save_snapshots s USING (
             SELECT id, row_number() OVER (PARTITION BY user_id, package_id ORDER BY id DESC) AS rank
             FROM save_snapshots) ranked
           WHERE s.id = ranked.id AND ranked.rank > $1
             AND NOT EXISTS (SELECT 1 FROM save_heads h WHERE h.snapshot_id = s.id)"#,
        RETAINED_SNAPSHOTS
    )
    .execute(&state.db)
    .await?
    .rows_affected();
    let orphans = sqlx::query!(
        r#"SELECT user_id, blake3, object_name FROM save_blobs b
           WHERE b.created_at < now() - interval '1 day'
             AND NOT EXISTS (SELECT 1 FROM save_snapshot_blobs sb WHERE sb.user_id = b.user_id AND sb.blake3 = b.blake3)
           LIMIT 10000"#
    )
    .fetch_all(&state.db)
    .await?;
    for o in &orphans {
        state
            .storage
            .delete(BucketKind::Saves, &o.object_name)
            .await?;
        sqlx::query!(
            r#"DELETE FROM save_blobs b WHERE user_id = $1 AND blake3 = $2
               AND NOT EXISTS (SELECT 1 FROM save_snapshot_blobs sb WHERE sb.user_id = b.user_id AND sb.blake3 = b.blake3)"#,
            o.user_id,
            &o.blake3
        )
        .execute(&state.db)
        .await?;
    }
    tracing::info!(snapshots = dropped, blobs = orphans.len(), "saves.gc done");
    Ok(())
}
