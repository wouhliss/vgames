//! Cloud saves (A1-T13, docs/architecture/06-cloud-saves.md §4).
//!
//! Blobs are content-addressed per user (`v1/{user_id}/{blake3}` in the saves bucket) and
//! move straight between the launcher and storage through signed URLs. A snapshot lists
//! files by blob; the head per (user, package) moves only by compare-and-swap on the
//! parent snapshot. A user only ever sees their own saves.
//!
//! Quota (`VGAMES_SAVE_QUOTA_BYTES_PER_PACKAGE`) counts distinct blob bytes: `prepare`
//! refuses a push when the head's blobs, the pushed blobs and the uploads still in flight
//! exceed it; a commit refuses a snapshot larger than the quota and then drops the oldest
//! history (never the head) until the retained snapshots of the package fit.
//!
//! `saves.gc` (daily) keeps the newest [`RETAINED_SNAPSHOTS`] snapshots plus the head and
//! deletes blobs no snapshot references. It takes a per-user advisory lock that `prepare`
//! and commits share, so a blob is never deleted between being confirmed and referenced.

use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use axum::{
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use time::OffsetDateTime;
use utoipa::IntoParams;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_proto::{
    FieldError,
    saves::{
        BlobDownloadUrl, BlobDownloadUrlList, BlobDownloadUrlsRequest, BlobUploadTarget,
        CommitSnapshotRequest, OsFamily, PrepareBlobsRequest, PrepareBlobsResponse, PutMethod,
        SaveSnapshot, SaveSnapshotPage, SaveSnapshotSummary,
    },
};

use crate::http::path::Path;
use crate::{
    auth::CurrentUser,
    error::{ApiError, ApiResult},
    http::{
        idempotency::{self, IdempotencyKey},
        json::{Json, Validate, invalid},
        pagination::{CursorCodec, PageParams, finish_page},
        query::Query,
    },
    openapi_problems::{BadRequest, Conflict, NotFound, PayloadTooLarge, Unauthorized},
    state::AppState,
    storage::BucketKind,
};

/// Signed PUT URLs for blob uploads live this long (01-security §5).
pub const UPLOAD_TTL: Duration = Duration::from_secs(15 * 60);
/// Snapshots kept per (user, package) besides the head.
pub const RETAINED_SNAPSHOTS: i64 = 20;
pub const MAX_FILES: usize = 10_000;
pub const MAX_BLOB_SIZE: i64 = 4_294_967_296;
pub const MAX_DOWNLOAD_URLS: usize = 1000;
/// Unreferenced blobs are kept this long after their last `prepare` or upload, so a push
/// in progress never loses a blob it was told the server already has.
pub const GC_GRACE: time::Duration = time::Duration::days(1);
/// Unconfirmed uploads count against the quota for this long after `prepare` (the signed
/// URLs expire after 15 minutes).
const PENDING_WINDOW_SECS: f64 = 3600.0;
/// Advisory lock class for per-user blob bookkeeping (`pg_advisory_*lock(class, hashtext(user))`).
const LOCK_CLASS: i32 = 0x5341_5645;
const HEAD_CONCURRENCY: usize = 16;
/// Validation stops listing problems after this many.
const MAX_REPORTED: usize = 50;

/// Body limits above the 1 MiB default: 10,000 file entries with long paths.
const COMMIT_BODY_LIMIT: usize = 8 * 1024 * 1024;
const PREPARE_BODY_LIMIT: usize = 2 * 1024 * 1024;

pub fn routes() -> OpenApiRouter<AppState> {
    let commit = OpenApiRouter::new()
        .routes(routes!(list_snapshots, commit_snapshot))
        .layer(DefaultBodyLimit::max(COMMIT_BODY_LIMIT));
    let prepare = OpenApiRouter::new()
        .routes(routes!(prepare_blobs))
        .layer(DefaultBodyLimit::max(PREPARE_BODY_LIMIT));
    OpenApiRouter::new()
        .routes(routes!(get_head))
        .routes(routes!(get_snapshot))
        .routes(routes!(blob_download_urls))
        .merge(commit)
        .merge(prepare)
}

// ---------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------

/// Decodes a lowercase-hex BLAKE3 digest.
fn parse_hash(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    hex::decode(s).ok()?.try_into().ok()
}

pub fn blob_object(user_id: Uuid, blake3_hex: &str) -> String {
    format!("v1/{user_id}/{blake3_hex}")
}

fn no_saves() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "no_saves",
        "There are no cloud saves for this game yet",
    )
}

fn quota_exceeded(needed: i64, quota: u64) -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "save_quota_exceeded",
        "These saves are larger than the cloud storage allowed per game",
    )
    .with_detail(format!(
        "This push needs {needed} bytes of cloud storage for this game; the limit is {quota} bytes."
    ))
}

fn not_uploaded(count: usize) -> ApiError {
    ApiError::conflict(
        "blob_not_uploaded",
        "Some save files were not uploaded; prepare and upload them again",
    )
    .with_detail(format!(
        "{count} referenced blob(s) are missing from storage."
    ))
}

fn quota(state: &AppState) -> i64 {
    i64::try_from(state.config.save_quota_bytes_per_package).unwrap_or(i64::MAX)
}

/// Saves can be written for packages that exist (any status: a hidden game stays playable).
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

/// Shared per-user lock held by `prepare` and commits; `saves.gc` takes it exclusively.
async fn lock_user_shared(conn: &mut PgConnection, user_id: Uuid) -> ApiResult<()> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock_shared($1, hashtext($2))",
        LOCK_CLASS,
        user_id.to_string()
    )
    .execute(conn)
    .await?;
    Ok(())
}

fn validate_blob(errors: &mut Vec<FieldError>, at: &str, blake3: &str, size: i64) {
    if parse_hash(blake3).is_none() {
        invalid(
            errors,
            &format!("{at}.blake3"),
            "invalid",
            "must be 64 lowercase hex characters",
        );
    }
    if !(0..=MAX_BLOB_SIZE).contains(&size) {
        invalid(
            errors,
            &format!("{at}.size"),
            "out_of_range",
            format!("must be 0..={MAX_BLOB_SIZE}"),
        );
    }
}

fn valid_root(root: &str) -> bool {
    (1..=32).contains(&root.len())
        && root
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

// ---------------------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------------------

impl Validate for PrepareBlobsRequest {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if self.blobs.len() > MAX_FILES {
            invalid(
                errors,
                "blobs",
                "max_items",
                format!("at most {MAX_FILES} blobs"),
            );
            return;
        }
        let mut sizes: HashMap<&str, i64> = HashMap::new();
        for (i, b) in self.blobs.iter().enumerate() {
            if errors.len() >= MAX_REPORTED {
                break;
            }
            validate_blob(errors, &format!("blobs[{i}]"), &b.blake3, b.size);
            if sizes.insert(&b.blake3, b.size).is_some_and(|s| s != b.size) {
                invalid(
                    errors,
                    &format!("blobs[{i}].size"),
                    "conflict",
                    "the same blob is listed with two sizes",
                );
            }
        }
    }
}

impl Validate for BlobDownloadUrlsRequest {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if !(1..=MAX_DOWNLOAD_URLS).contains(&self.blake3.len()) {
            invalid(
                errors,
                "blake3",
                "count",
                format!("ask for 1-{MAX_DOWNLOAD_URLS} blobs"),
            );
            return;
        }
        let mut seen = std::collections::HashSet::new();
        for (i, h) in self.blake3.iter().enumerate() {
            if errors.len() >= MAX_REPORTED {
                break;
            }
            if parse_hash(h).is_none() {
                invalid(
                    errors,
                    &format!("blake3[{i}]"),
                    "invalid",
                    "must be 64 lowercase hex characters",
                );
            } else if !seen.insert(h.as_str()) {
                invalid(
                    errors,
                    &format!("blake3[{i}]"),
                    "duplicate",
                    "listed more than once",
                );
            }
        }
    }
}

impl Validate for CommitSnapshotRequest {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if self.label.as_ref().is_some_and(|l| l.chars().count() > 100) {
            invalid(
                errors,
                "label",
                "max_length",
                "must be at most 100 characters",
            );
        }
        if self.files.len() > MAX_FILES {
            invalid(
                errors,
                "files",
                "max_items",
                format!("at most {MAX_FILES} files"),
            );
            return;
        }
        let mut sizes: HashMap<&str, i64> = HashMap::new();
        let mut by_root: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (i, f) in self.files.iter().enumerate() {
            if errors.len() >= MAX_REPORTED {
                return;
            }
            let at = format!("files[{i}]");
            if !valid_root(&f.root) {
                invalid(
                    errors,
                    &format!("{at}.root"),
                    "invalid",
                    "must match ^[a-z0-9_-]{1,32}$",
                );
            }
            if f.path.chars().count() > 512 {
                invalid(
                    errors,
                    &format!("{at}.path"),
                    "max_length",
                    "must be at most 512 characters",
                );
            } else if let Err(e) = vgames_core::paths::validate_path(&f.path) {
                invalid(errors, &format!("{at}.path"), "invalid_path", e.to_string());
            }
            validate_blob(errors, &at, &f.blake3, f.size);
            if sizes.insert(&f.blake3, f.size).is_some_and(|s| s != f.size) {
                invalid(
                    errors,
                    &format!("{at}.size"),
                    "conflict",
                    "another file has the same content hash with a different size",
                );
            }
            by_root.entry(&f.root).or_default().push(&f.path);
        }
        if !errors.is_empty() {
            return;
        }
        for (root, paths) in by_root {
            if let Err(e) = vgames_core::paths::validate_tree(paths, std::iter::empty()) {
                invalid(
                    errors,
                    "files",
                    "path_conflict",
                    format!("save location {root}: {e}"),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Reading
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
    created_at: OffsetDateTime,
}

impl SnapshotRow {
    fn summary(self) -> ApiResult<SaveSnapshotSummary> {
        Ok(SaveSnapshotSummary {
            id: self.id,
            package_id: self.package_id,
            parent_id: self.parent_id,
            device_id: self.device_id,
            device_name: self.device_name,
            platform: OsFamily::parse(&self.platform).ok_or_else(ApiError::internal)?,
            file_count: self.file_count,
            total_size: self.total_size,
            label: self.label,
            created_at: self.created_at,
        })
    }
}

fn with_files(row: SnapshotRow, files: serde_json::Value) -> ApiResult<SaveSnapshot> {
    Ok(SaveSnapshot {
        summary: row.summary()?,
        files: serde_json::from_value(files).map_err(ApiError::internal_from)?,
    })
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
    let r = sqlx::query!(
        r#"SELECT s.id, s.package_id, s.parent_id, s.device_id, d.display_name AS "device_name?",
                  s.platform, s.file_count, s.total_size, s.label, s.created_at, s.files
           FROM save_heads h
           JOIN save_snapshots s ON s.id = h.snapshot_id
           LEFT JOIN devices d ON d.id = s.device_id
           WHERE h.user_id = $1 AND h.package_id = $2"#,
        user.user_id,
        package_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(no_saves)?;
    let row = SnapshotRow {
        id: r.id,
        package_id: r.package_id,
        parent_id: r.parent_id,
        device_id: r.device_id,
        device_name: r.device_name,
        platform: r.platform,
        file_count: r.file_count,
        total_size: r.total_size,
        label: r.label,
        created_at: r.created_at,
    };
    Ok(axum::Json(with_files(row, r.files)?))
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
    let filters = json!({ "saves": package_id, "user": user.user_id });
    let codec = CursorCodec::new(state.keys.cursor.expose());
    let after: Option<Uuid> = q
        .cursor
        .as_deref()
        .map(|c| codec.decode(c, &filters))
        .transpose()?
        .map(|(_, id): (String, Uuid)| id);
    // UUIDv7 ids sort by creation time.
    let rows = sqlx::query_as!(
        SnapshotRow,
        r#"SELECT s.id, s.package_id, s.parent_id, s.device_id, d.display_name AS "device_name?",
                  s.platform, s.file_count, s.total_size, s.label, s.created_at
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
    Ok(axum::Json(SaveSnapshotPage {
        items: page
            .items
            .into_iter()
            .map(SnapshotRow::summary)
            .collect::<ApiResult<_>>()?,
        next_cursor: page.next_cursor,
    }))
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
    let r = sqlx::query!(
        r#"SELECT s.id, s.package_id, s.parent_id, s.device_id, d.display_name AS "device_name?",
                  s.platform, s.file_count, s.total_size, s.label, s.created_at, s.files
           FROM save_snapshots s LEFT JOIN devices d ON d.id = s.device_id
           WHERE s.id = $1 AND s.user_id = $2 AND s.package_id = $3"#,
        snapshot_id,
        user.user_id,
        package_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let row = SnapshotRow {
        id: r.id,
        package_id: r.package_id,
        parent_id: r.parent_id,
        device_id: r.device_id,
        device_name: r.device_name,
        platform: r.platform,
        file_count: r.file_count,
        total_size: r.total_size,
        label: r.label,
        created_at: r.created_at,
    };
    Ok(axum::Json(with_files(row, r.files)?))
}

// ---------------------------------------------------------------------------------------
// Blobs
// ---------------------------------------------------------------------------------------

/// Distinct `(hash bytes, size, hex)` of a validated blob list.
fn distinct_blobs<'a>(
    items: impl IntoIterator<Item = (&'a str, i64)>,
) -> ApiResult<Vec<([u8; 32], i64, &'a str)>> {
    let mut map: BTreeMap<&str, i64> = BTreeMap::new();
    for (h, size) in items {
        map.insert(h, size);
    }
    map.into_iter()
        .map(|(h, size)| {
            let bytes = parse_hash(h).ok_or_else(ApiError::internal)?;
            Ok((bytes, size, h))
        })
        .collect()
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
        BadRequest, Unauthorized, NotFound, PayloadTooLarge
    )
)]
pub async fn prepare_blobs(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(package_id): Path<Uuid>,
    Json(req): Json<PrepareBlobsRequest>,
) -> ApiResult<axum::Json<PrepareBlobsResponse>> {
    ensure_package(&state, package_id).await?;
    let blobs = distinct_blobs(req.blobs.iter().map(|b| (b.blake3.as_str(), b.size)))?;
    if blobs.is_empty() {
        return Ok(axum::Json(PrepareBlobsResponse {
            missing: Vec::new(),
        }));
    }
    let hashes: Vec<Vec<u8>> = blobs.iter().map(|(h, _, _)| h.to_vec()).collect();
    let sizes: Vec<i64> = blobs.iter().map(|(_, s, _)| *s).collect();
    let names: Vec<String> = blobs
        .iter()
        .map(|(_, _, hex)| blob_object(user.user_id, hex))
        .collect();

    let mut tx = state.db.begin().await?;
    lock_user_shared(&mut tx, user.user_id).await?;
    // `created_at` doubles as "last prepared": a blob the client was just told about stays
    // out of garbage collection for GC_GRACE.
    let rows = sqlx::query!(
        r#"INSERT INTO save_blobs (user_id, blake3, size, object_name)
           SELECT $1, t.h, t.s, t.n FROM unnest($2::bytea[], $3::int8[], $4::text[]) AS t(h, s, n)
           ON CONFLICT (user_id, blake3) DO UPDATE
             SET created_at = now(),
                 size = CASE WHEN save_blobs.uploaded_at IS NULL THEN EXCLUDED.size ELSE save_blobs.size END
           RETURNING blake3, size, object_name, uploaded_at IS NOT NULL AS "uploaded!""#,
        user.user_id,
        &hashes,
        &sizes,
        &names
    )
    .fetch_all(&mut *tx)
    .await?;
    let wanted: HashMap<[u8; 32], i64> = blobs.iter().map(|(h, s, _)| (*h, *s)).collect();
    let mut missing = Vec::new();
    for r in rows {
        let hash: [u8; 32] = r
            .blake3
            .as_slice()
            .try_into()
            .map_err(ApiError::internal_from)?;
        if r.uploaded && wanted.get(&hash) != Some(&r.size) {
            return Err(ApiError::field(
                "blobs",
                "size_mismatch",
                format!("blob {} is stored with size {}", hex::encode(hash), r.size),
            ));
        }
        if !r.uploaded {
            missing.push((hex::encode(hash), r.object_name, r.size));
        }
    }
    let usage = sqlx::query_scalar!(
        r#"SELECT COALESCE(sum(b.size), 0)::int8 AS "usage!" FROM save_blobs b
           WHERE b.user_id = $1 AND (
             b.blake3 = ANY($3)
             OR (b.uploaded_at IS NULL AND b.created_at > now() - make_interval(secs => $4))
             OR b.blake3 IN (SELECT sb.blake3 FROM save_heads h
                             JOIN save_snapshot_blobs sb ON sb.snapshot_id = h.snapshot_id
                             WHERE h.user_id = $1 AND h.package_id = $2))"#,
        user.user_id,
        package_id,
        &hashes,
        PENDING_WINDOW_SECS
    )
    .fetch_one(&mut *tx)
    .await?;
    if usage > quota(&state) {
        return Err(quota_exceeded(
            usage,
            state.config.save_quota_bytes_per_package,
        ));
    }
    tx.commit().await?;

    let mut targets = Vec::with_capacity(missing.len());
    for (hex, object, size) in missing {
        let signed = state
            .storage
            .sign_put(
                BucketKind::Saves,
                &object,
                UPLOAD_TTL,
                "application/octet-stream",
                u64::try_from(size).map_err(ApiError::internal_from)?,
            )
            .await?;
        targets.push(BlobUploadTarget {
            blake3: hex,
            url: signed.url,
            method: PutMethod::Put,
            headers: signed.headers.into_iter().collect(),
            expires_at: signed.expires_at,
        });
    }
    tracing::debug!(%package_id, missing = targets.len(), "save blob upload targets issued");
    Ok(axum::Json(PrepareBlobsResponse { missing: targets }))
}

/// Signed GET URLs for blobs
#[utoipa::path(
    post,
    path = "/v1/saves/{package_id}/blobs/download-urls",
    tag = "saves",
    operation_id = "createSaveBlobDownloadUrls",
    params(("package_id" = Uuid, Path)),
    request_body = BlobDownloadUrlsRequest,
    responses(
        (status = 200, description = "URLs", body = inline(BlobDownloadUrlList)),
        BadRequest, Unauthorized, NotFound
    )
)]
pub async fn blob_download_urls(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(package_id): Path<Uuid>,
    Json(req): Json<BlobDownloadUrlsRequest>,
) -> ApiResult<axum::Json<BlobDownloadUrlList>> {
    let hashes: Vec<Vec<u8>> = req
        .blake3
        .iter()
        .filter_map(|h| parse_hash(h).map(|b| b.to_vec()))
        .collect();
    // Only blobs of the caller's own snapshots of this package.
    let rows = sqlx::query!(
        r#"SELECT b.blake3, b.object_name FROM save_blobs b
           WHERE b.user_id = $1 AND b.blake3 = ANY($3) AND b.uploaded_at IS NOT NULL
             AND EXISTS (SELECT 1 FROM save_snapshot_blobs sb
                         JOIN save_snapshots s ON s.id = sb.snapshot_id
                         WHERE sb.user_id = b.user_id AND sb.blake3 = b.blake3 AND s.package_id = $2)
           ORDER BY b.blake3"#,
        user.user_id,
        package_id,
        &hashes
    )
    .fetch_all(&state.db)
    .await?;
    if rows.len() != hashes.len() {
        return Err(ApiError::not_found().with_detail(format!(
            "{} of the requested blobs are not part of your saves for this game.",
            hashes.len() - rows.len()
        )));
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
        items.push(BlobDownloadUrl {
            blake3: hex::encode(&r.blake3),
            url: signed.url,
            expires_at: signed.expires_at,
        });
    }
    Ok(axum::Json(BlobDownloadUrlList { items }))
}

// ---------------------------------------------------------------------------------------
// Commit
// ---------------------------------------------------------------------------------------

/// Confirms unconfirmed blobs through storage metadata (exists, exact size). Returns how
/// many are still missing.
async fn confirm_uploads(
    state: &AppState,
    user_id: Uuid,
    hashes: &[Vec<u8>],
    wanted: &HashMap<Vec<u8>, i64>,
) -> ApiResult<usize> {
    let rows = sqlx::query!(
        r#"SELECT blake3, size, object_name, uploaded_at IS NOT NULL AS "uploaded!"
           FROM save_blobs WHERE user_id = $1 AND blake3 = ANY($2)"#,
        user_id,
        hashes
    )
    .fetch_all(&state.db)
    .await?;
    let mut missing = hashes.len() - rows.len();
    let mut to_check = Vec::new();
    for r in rows {
        if wanted.get(&r.blake3) != Some(&r.size) {
            return Err(ApiError::field(
                "files",
                "size_mismatch",
                format!(
                    "blob {} was prepared with size {}",
                    hex::encode(&r.blake3),
                    r.size
                ),
            ));
        }
        if !r.uploaded {
            to_check.push((r.blake3, r.object_name, r.size));
        }
    }
    let results: Vec<ApiResult<Option<Vec<u8>>>> = stream::iter(to_check)
        .map(|(hash, object, size)| async move {
            let meta = state.storage.head(BucketKind::Saves, &object).await?;
            let ok = meta.is_some_and(|m| i64::try_from(m.size).ok() == Some(size));
            Ok(ok.then_some(hash))
        })
        .buffer_unordered(HEAD_CONCURRENCY)
        .collect()
        .await;
    let mut confirmed = Vec::new();
    for r in results {
        match r? {
            Some(hash) => confirmed.push(hash),
            None => missing += 1,
        }
    }
    if !confirmed.is_empty() {
        sqlx::query!(
            "UPDATE save_blobs SET uploaded_at = now() WHERE user_id = $1 AND blake3 = ANY($2) AND uploaded_at IS NULL",
            user_id,
            &confirmed
        )
        .execute(&state.db)
        .await?;
    }
    Ok(missing)
}

fn head_conflict(expected: Option<Uuid>, current: Option<Uuid>) -> ApiError {
    let name = |id: Option<Uuid>| id.map_or_else(|| "none".to_string(), |i| i.to_string());
    ApiError::conflict("save_head_conflict", "Cloud save changed on another device")
        .with_detail(format!(
            "Expected head {} but the current head is {}.",
            name(expected),
            name(current)
        ))
        .with_errors(vec![FieldError {
            field: "parent_snapshot_id".into(),
            code: "stale".into(),
            message: Some(name(current)),
        }])
}

/// Distinct blob bytes of every snapshot of (user, package).
async fn retained_usage(
    conn: &mut PgConnection,
    user_id: Uuid,
    package_id: Uuid,
) -> ApiResult<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT COALESCE(sum(b.size), 0)::int8 AS "usage!" FROM save_blobs b
           WHERE b.user_id = $1 AND b.blake3 IN (
             SELECT sb.blake3 FROM save_snapshots s JOIN save_snapshot_blobs sb ON sb.snapshot_id = s.id
             WHERE s.user_id = $1 AND s.package_id = $2)"#,
        user_id,
        package_id
    )
    .fetch_one(conn)
    .await?)
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
        BadRequest, Unauthorized, NotFound, Conflict, PayloadTooLarge
    )
)]
pub async fn commit_snapshot(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(package_id): Path<Uuid>,
    key: IdempotencyKey,
    Json(req): Json<CommitSnapshotRequest>,
) -> ApiResult<Response> {
    let fp = idempotency::fingerprint("POST", &format!("/v1/saves/{package_id}/snapshots"), &req);
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
    ensure_package(state, package_id).await?;
    let blobs = distinct_blobs(req.files.iter().map(|f| (f.blake3.as_str(), f.size)))?;
    let needed: i64 = blobs.iter().map(|(_, s, _)| *s).sum();
    if needed > quota(state) {
        return Err(quota_exceeded(
            needed,
            state.config.save_quota_bytes_per_package,
        ));
    }
    let hashes: Vec<Vec<u8>> = blobs.iter().map(|(h, _, _)| h.to_vec()).collect();
    let wanted: HashMap<Vec<u8>, i64> = blobs.iter().map(|(h, s, _)| (h.to_vec(), *s)).collect();
    let missing = confirm_uploads(state, user.user_id, &hashes, &wanted).await?;
    if missing > 0 {
        return Err(not_uploaded(missing));
    }

    let total_size: i64 = req.files.iter().map(|f| f.size).sum();
    let file_count = i32::try_from(req.files.len()).map_err(ApiError::internal_from)?;
    let label = req
        .label
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string);
    let files_json = serde_json::to_value(&req.files).map_err(ApiError::internal_from)?;

    let mut tx = state.db.begin().await?;
    lock_user_shared(&mut tx, user.user_id).await?;
    // Garbage collection cannot run while we hold the shared lock, so blobs confirmed now
    // are still there when the snapshot references them.
    let confirmed = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM save_blobs WHERE user_id = $1 AND blake3 = ANY($2) AND uploaded_at IS NOT NULL"#,
        user.user_id,
        &hashes
    )
    .fetch_one(&mut *tx)
    .await?;
    let confirmed = usize::try_from(confirmed).unwrap_or(0);
    if confirmed != hashes.len() {
        return Err(not_uploaded(hashes.len().saturating_sub(confirmed)));
    }
    // Serializes commits of this (user, package): the second waits here, then sees the new head.
    let head = sqlx::query_scalar!(
        "SELECT snapshot_id FROM save_heads WHERE user_id = $1 AND package_id = $2 FOR UPDATE",
        user.user_id,
        package_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    if head != req.parent_snapshot_id {
        return Err(head_conflict(req.parent_snapshot_id, head));
    }
    let row = sqlx::query!(
        r#"INSERT INTO save_snapshots (user_id, package_id, parent_id, device_id, platform, file_count, total_size, files, label)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
           RETURNING id, created_at"#,
        user.user_id,
        package_id,
        req.parent_snapshot_id,
        user.device_id,
        req.platform.as_str(),
        file_count,
        total_size,
        files_json,
        label
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        r#"INSERT INTO save_snapshot_blobs (snapshot_id, user_id, blake3)
           SELECT $1, $2, h FROM unnest($3::bytea[]) AS h"#,
        row.id,
        user.user_id,
        &hashes
    )
    .execute(&mut *tx)
    .await?;
    let moved = match req.parent_snapshot_id {
        Some(parent) => sqlx::query!(
            r#"UPDATE save_heads SET snapshot_id = $3, updated_at = now()
               WHERE user_id = $1 AND package_id = $2 AND snapshot_id = $4"#,
            user.user_id,
            package_id,
            row.id,
            parent
        )
        .execute(&mut *tx)
        .await?
        .rows_affected(),
        None => sqlx::query!(
            r#"INSERT INTO save_heads (user_id, package_id, snapshot_id) VALUES ($1, $2, $3)
               ON CONFLICT (user_id, package_id) DO NOTHING"#,
            user.user_id,
            package_id,
            row.id
        )
        .execute(&mut *tx)
        .await?
        .rows_affected(),
    };
    if moved != 1 {
        let current = sqlx::query_scalar!(
            "SELECT snapshot_id FROM save_heads WHERE user_id = $1 AND package_id = $2",
            user.user_id,
            package_id
        )
        .fetch_optional(&state.db)
        .await?;
        return Err(head_conflict(req.parent_snapshot_id, current));
    }
    trim_to_quota(&mut tx, state, user.user_id, package_id, row.id).await?;
    tx.commit().await?;

    let device_name = match user.device_id {
        Some(d) => {
            sqlx::query_scalar!("SELECT display_name FROM devices WHERE id = $1", d)
                .fetch_optional(&state.db)
                .await?
        }
        None => None,
    };
    tracing::info!(%package_id, snapshot_id = %row.id, files = file_count, "cloud save committed");
    Ok(SaveSnapshot {
        summary: SaveSnapshotSummary {
            id: row.id,
            package_id,
            parent_id: req.parent_snapshot_id,
            device_id: user.device_id,
            device_name,
            platform: req.platform,
            file_count,
            total_size,
            label,
            created_at: row.created_at,
        },
        files: req.files,
    })
}

/// Drops the oldest snapshots (never `head`) while the package's retained blobs exceed the quota.
async fn trim_to_quota(
    tx: &mut PgConnection,
    state: &AppState,
    user_id: Uuid,
    package_id: Uuid,
    head: Uuid,
) -> ApiResult<()> {
    let limit = quota(state);
    // Bounded: each round drops one snapshot; the next commit continues if needed.
    for _ in 0..200 {
        if retained_usage(tx, user_id, package_id).await? <= limit {
            return Ok(());
        }
        let dropped = sqlx::query_scalar!(
            r#"DELETE FROM save_snapshots WHERE id = (
                 SELECT id FROM save_snapshots
                 WHERE user_id = $1 AND package_id = $2 AND id <> $3
                 ORDER BY id LIMIT 1)
               RETURNING id"#,
            user_id,
            package_id,
            head
        )
        .fetch_optional(&mut *tx)
        .await?;
        match dropped {
            Some(id) => {
                tracing::info!(%package_id, snapshot_id = %id, "old cloud save dropped to stay within quota")
            }
            None => return Ok(()),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Retention and garbage collection (`saves.gc`)
// ---------------------------------------------------------------------------------------

/// What one GC run removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GcReport {
    pub snapshots: u64,
    pub blobs: u64,
}

const GC_BATCH: i64 = 1000;

/// Applies snapshot retention, then deletes unreferenced blobs (rows and objects).
pub async fn gc(state: &AppState) -> ApiResult<GcReport> {
    let mut report = GcReport::default();
    loop {
        let n = sqlx::query!(
            r#"DELETE FROM save_snapshots WHERE id IN (
                 SELECT r.id FROM (
                   SELECT s.id, row_number() OVER (PARTITION BY s.user_id, s.package_id ORDER BY s.id DESC) AS rn
                   FROM save_snapshots s) r
                 WHERE r.rn > $1
                   AND NOT EXISTS (SELECT 1 FROM save_heads h WHERE h.snapshot_id = r.id)
                 LIMIT $2)"#,
            RETAINED_SNAPSHOTS,
            GC_BATCH
        )
        .execute(&state.db)
        .await?
        .rows_affected();
        report.snapshots += n;
        if n < GC_BATCH as u64 {
            break;
        }
    }
    let grace = GC_GRACE.as_seconds_f64();
    loop {
        let users = sqlx::query_scalar!(
            r#"SELECT DISTINCT b.user_id FROM save_blobs b
               WHERE b.created_at < now() - make_interval(secs => $1)
                 AND (b.uploaded_at IS NULL OR b.uploaded_at < now() - make_interval(secs => $1))
                 AND NOT EXISTS (SELECT 1 FROM save_snapshot_blobs sb WHERE sb.user_id = b.user_id AND sb.blake3 = b.blake3)
               LIMIT 100"#,
            grace
        )
        .fetch_all(&state.db)
        .await?;
        if users.is_empty() {
            break;
        }
        let mut removed_this_round = 0;
        for user_id in users {
            let n = gc_user_blobs(state, user_id, grace).await?;
            removed_this_round += n;
            report.blobs += n;
        }
        if removed_this_round == 0 {
            break;
        }
    }
    Ok(report)
}

/// Deletes one user's unreferenced blobs while holding their lock exclusively.
async fn gc_user_blobs(state: &AppState, user_id: Uuid, grace: f64) -> ApiResult<u64> {
    // The lock lives in its own transaction for the whole run, including object deletes;
    // dropping it on error rolls back and releases the lock.
    let mut lock = state.db.begin().await?;
    sqlx::query!(
        "SELECT pg_advisory_xact_lock($1, hashtext($2))",
        LOCK_CLASS,
        user_id.to_string()
    )
    .execute(&mut *lock)
    .await?;
    let mut removed = 0u64;
    loop {
        // Rows go first (committed); objects after. A crash in between leaves orphan
        // objects (a storage leak), never a row whose object is gone.
        let objects = sqlx::query_scalar!(
            r#"DELETE FROM save_blobs WHERE user_id = $1 AND blake3 IN (
                 SELECT b.blake3 FROM save_blobs b
                 WHERE b.user_id = $1
                   AND b.created_at < now() - make_interval(secs => $2)
                   AND (b.uploaded_at IS NULL OR b.uploaded_at < now() - make_interval(secs => $2))
                   AND NOT EXISTS (SELECT 1 FROM save_snapshot_blobs sb WHERE sb.user_id = b.user_id AND sb.blake3 = b.blake3)
                 LIMIT $3)
               RETURNING object_name"#,
            user_id,
            grace,
            GC_BATCH
        )
        .fetch_all(&state.db)
        .await?;
        let n = objects.len();
        for object in objects {
            if let Err(e) = state.storage.delete(BucketKind::Saves, &object).await {
                tracing::warn!(error = %e, "could not delete a cloud-save blob object; it is orphaned");
            }
        }
        removed += n as u64;
        if n < usize::try_from(GC_BATCH).unwrap_or(usize::MAX) {
            break;
        }
    }
    lock.commit().await?;
    Ok(removed)
}
