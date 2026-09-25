//! Package images (A1-T09).
//!
//! Uploads are sniffed by content, decoded under strict limits (≤ 16384², bounded
//! allocation: decompression bombs fail), re-encoded to strip metadata, deduplicated by
//! SHA-256 per package and stored in the assets bucket. Clients fetch them through
//! `GET /v1/assets/{id}`, a redirect to a short-lived signed URL.

use std::{io::Cursor, time::Duration};

use axum::{
    extract::{DefaultBodyLimit, Multipart, State, multipart::MultipartRejection},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use image::{ImageFormat, ImageReader, Limits};
use serde_json::json;
use sha2::{Digest, Sha256};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_proto::packages::{Asset, AssetKind, AssetSource, ImageType};

use crate::http::path::Path;
use crate::{
    audit,
    auth::{CurrentUser, RequestMeta, RequireAdmin},
    error::{ApiError, ApiResult},
    openapi_problems::{
        BadRequest, Forbidden, NotFound, PayloadTooLarge, Unauthorized, UnsupportedMediaType,
    },
    packages::{asset_kind_str, parse_asset_kind},
    state::AppState,
    storage::BucketKind,
};

pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_DIMENSION: u32 = 16_384;
/// Decoder allocation cap (a 16384² RGBA image is 1 GiB; real covers are far smaller).
const MAX_ALLOC: u64 = 512 * 1024 * 1024;
const REDIRECT_TTL: Duration = Duration::from_secs(3600);

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(upload_asset))
        .routes(routes!(delete_asset))
        .routes(routes!(get_asset))
        // Multipart overhead on top of the 10 MiB image.
        .layer(DefaultBodyLimit::max(MAX_IMAGE_BYTES + 256 * 1024))
}

/// A decoded, re-encoded image ready to store.
pub struct ProcessedImage {
    pub bytes: Bytes,
    pub content_type: &'static str,
    pub ext: &'static str,
    pub width: u32,
    pub height: u32,
    pub sha256: [u8; 32],
}

/// Validates and re-encodes an image. Shared with the metadata image job (A1-T10).
pub fn process_image(raw: &[u8]) -> ApiResult<ProcessedImage> {
    if raw.len() > MAX_IMAGE_BYTES {
        return Err(ApiError::payload_too_large().with_detail("Images must be at most 10 MiB"));
    }
    let unsupported =
        || ApiError::unsupported_media_type().with_detail("Use a JPEG, PNG or WebP image");
    let format = image::guess_format(raw).map_err(|_| unsupported())?;
    let (content_type, ext) = match format {
        ImageFormat::Jpeg => ("image/jpeg", "jpg"),
        ImageFormat::Png => ("image/png", "png"),
        ImageFormat::WebP => ("image/webp", "webp"),
        _ => return Err(unsupported()),
    };
    let mut reader = ImageReader::with_format(Cursor::new(raw), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| {
        ApiError::bad_request("invalid_image", "The image could not be decoded")
            .with_detail(e.to_string())
    })?;
    let (width, height) = (img.width(), img.height());

    let mut out = Cursor::new(Vec::new());
    match format {
        ImageFormat::Jpeg => {
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
            img.to_rgb8()
                .write_with_encoder(enc)
                .map_err(ApiError::internal_from)?;
        }
        ImageFormat::Png => img
            .write_to(&mut out, ImageFormat::Png)
            .map_err(ApiError::internal_from)?,
        _ => {
            let enc = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
            img.to_rgba8()
                .write_with_encoder(enc)
                .map_err(ApiError::internal_from)?;
        }
    }
    let bytes = out.into_inner();
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(
            ApiError::payload_too_large().with_detail("The image is too large once re-encoded")
        );
    }
    Ok(ProcessedImage {
        sha256: Sha256::digest(&bytes).into(),
        bytes: Bytes::from(bytes),
        content_type,
        ext,
        width,
        height,
    })
}

/// Stores a processed image for a package, returning the asset (existing one on duplicates).
pub async fn store_image(
    state: &AppState,
    package_id: Uuid,
    kind: AssetKind,
    img: ProcessedImage,
    source: AssetSource,
    source_url: Option<&str>,
) -> ApiResult<(Asset, bool)> {
    if let Some(r) = sqlx::query!(
        "SELECT id, kind, width, height, content_type, source FROM package_assets WHERE package_id = $1 AND sha256 = $2",
        package_id,
        &img.sha256[..]
    )
    .fetch_optional(&state.db)
    .await?
    {
        return Ok((dto(r.id, &r.kind, r.width, r.height, &r.content_type, &r.source), false));
    }
    let id = Uuid::now_v7();
    let object = format!("v1/{package_id}/{id}.{}", img.ext);
    state
        .storage
        .put_small(
            BucketKind::Assets,
            &object,
            img.bytes.clone(),
            img.content_type,
        )
        .await?;
    let source_str = match source {
        AssetSource::Igdb => "igdb",
        AssetSource::Steam => "steam",
        AssetSource::Upload => "upload",
    };
    let inserted = sqlx::query!(
        r#"INSERT INTO package_assets (id, package_id, kind, object_name, content_type, width, height, byte_size, sha256,
                                       source, source_url, position)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                   COALESCE((SELECT max(position) + 1 FROM package_assets WHERE package_id = $2 AND kind = $3), 0))
           ON CONFLICT (package_id, sha256) DO NOTHING"#,
        id,
        package_id,
        asset_kind_str(kind),
        object,
        img.content_type,
        img.width as i32,
        img.height as i32,
        img.bytes.len() as i32,
        &img.sha256[..],
        source_str,
        source_url
    )
    .execute(&state.db)
    .await?;
    if inserted.rows_affected() == 0 {
        // Lost a race with an identical upload; keep the other one.
        let _ = state.storage.delete(BucketKind::Assets, &object).await;
        return Err(ApiError::conflict(
            "duplicate_asset",
            "This image was just uploaded; reload",
        ));
    }
    // A package's first cover/hero/logo becomes its default.
    let column = match kind {
        AssetKind::Cover => Some("cover"),
        AssetKind::Hero => Some("hero"),
        AssetKind::Logo => Some("logo"),
        _ => None,
    };
    if let Some(col) = column {
        sqlx::query!(
            r#"UPDATE packages SET
                 cover_asset_id = CASE WHEN $2 = 'cover' THEN COALESCE(cover_asset_id, $3) ELSE cover_asset_id END,
                 hero_asset_id = CASE WHEN $2 = 'hero' THEN COALESCE(hero_asset_id, $3) ELSE hero_asset_id END,
                 logo_asset_id = CASE WHEN $2 = 'logo' THEN COALESCE(logo_asset_id, $3) ELSE logo_asset_id END
               WHERE id = $1"#,
            package_id,
            col,
            id
        )
        .execute(&state.db)
        .await?;
    }
    Ok((
        dto(
            id,
            asset_kind_str(kind),
            img.width as i32,
            img.height as i32,
            img.content_type,
            source_str,
        ),
        true,
    ))
}

fn dto(id: Uuid, kind: &str, w: i32, h: i32, ct: &str, source: &str) -> Asset {
    Asset {
        id,
        kind: parse_asset_kind(kind).unwrap_or(AssetKind::Screenshot),
        url: format!("/v1/assets/{id}"),
        width: w,
        height: h,
        content_type: match ct {
            "image/png" => ImageType::Png,
            "image/webp" => ImageType::Webp,
            _ => ImageType::Jpeg,
        },
        source: Some(match source {
            "igdb" => AssetSource::Igdb,
            "steam" => AssetSource::Steam,
            _ => AssetSource::Upload,
        }),
    }
}

/// Multipart body for `adminUploadAsset` (documentation only).
#[derive(utoipa::ToSchema)]
#[allow(dead_code)]
pub struct AssetUpload {
    kind: AssetKind,
    #[schema(value_type = String, content_media_type = "application/octet-stream")]
    file: Vec<u8>,
}

/// Upload a custom image (JPEG/PNG/WebP, ≤ 10 MiB)
#[utoipa::path(
    post,
    path = "/v1/admin/packages/{package_id}/assets",
    tag = "admin-packages",
    operation_id = "adminUploadAsset",
    params(("package_id" = Uuid, Path)),
    request_body(content = AssetUpload, content_type = "multipart/form-data"),
    responses(
        (status = 201, description = "Stored", body = Asset),
        BadRequest, Unauthorized, Forbidden, NotFound, PayloadTooLarge, UnsupportedMediaType
    )
)]
pub async fn upload_asset(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Path(package_id): Path<Uuid>,
    multipart: Result<Multipart, MultipartRejection>,
) -> ApiResult<Response> {
    // axum's own rejection is plain text; every error here is problem+json.
    let mut multipart = multipart.map_err(|_| {
        ApiError::unsupported_media_type()
            .with_detail("Send the image as multipart/form-data with a boundary")
    })?;
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM packages WHERE id = $1 AND deleted_at IS NULL) AS "e!""#,
        package_id
    )
    .fetch_one(&state.db)
    .await?;
    if !exists {
        return Err(ApiError::not_found());
    }
    let bad = |msg: &str| {
        ApiError::bad_request("invalid_multipart", "The upload form is invalid")
            .with_detail(msg.to_string())
    };
    let (mut kind, mut file) = (None, None);
    while let Some(field) = multipart.next_field().await.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::payload_too_large()
        } else {
            bad(&e.body_text())
        }
    })? {
        match field.name() {
            Some("kind") => {
                let text = field.text().await.map_err(|e| bad(&e.body_text()))?;
                kind = Some(parse_asset_kind(text.trim()).ok_or_else(|| {
                    ApiError::field(
                        "kind",
                        "invalid",
                        "must be cover, hero, logo, screenshot or icon",
                    )
                })?);
            }
            Some("file") => {
                let data = field.bytes().await.map_err(|e| {
                    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
                        ApiError::payload_too_large()
                    } else {
                        bad(&e.body_text())
                    }
                })?;
                file = Some(data);
            }
            _ => return Err(bad("only the fields `kind` and `file` are accepted")),
        }
    }
    let kind = kind.ok_or_else(|| ApiError::field("kind", "required", "is required"))?;
    let file = file.ok_or_else(|| ApiError::field("file", "required", "is required"))?;
    let img = tokio::task::spawn_blocking(move || process_image(&file))
        .await
        .map_err(ApiError::internal_from)??;
    let (asset, created) =
        store_image(&state, package_id, kind, img, AssetSource::Upload, None).await?;
    if created {
        let mut tx = state.db.begin().await?;
        audit::record(
            &mut tx,
            admin.user_id,
            &meta,
            "asset.create",
            "package",
            &package_id.to_string(),
            json!({ "asset_id": asset.id, "kind": asset_kind_str(kind) }),
        )
        .await?;
        tx.commit().await?;
    }
    Ok((StatusCode::CREATED, axum::Json(asset)).into_response())
}

/// Delete an image
#[utoipa::path(
    delete,
    path = "/v1/admin/assets/{asset_id}",
    tag = "admin-packages",
    operation_id = "adminDeleteAsset",
    params(("asset_id" = Uuid, Path)),
    responses((status = 204, description = "Deleted"), Unauthorized, Forbidden, NotFound)
)]
pub async fn delete_asset(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Path(asset_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut tx = state.db.begin().await?;
    let row = sqlx::query!(
        "DELETE FROM package_assets WHERE id = $1 RETURNING package_id, object_name",
        asset_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    audit::record(
        &mut tx,
        admin.user_id,
        &meta,
        "asset.delete",
        "package",
        &row.package_id.to_string(),
        json!({ "asset_id": asset_id }),
    )
    .await?;
    tx.commit().await?;
    if let Err(e) = state
        .storage
        .delete(BucketKind::Assets, &row.object_name)
        .await
    {
        tracing::warn!(error = %e, %asset_id, "asset object not deleted; it is orphaned");
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Redirect to a signed image URL
#[utoipa::path(
    get,
    path = "/v1/assets/{asset_id}",
    tag = "catalog",
    operation_id = "getAsset",
    params(("asset_id" = Uuid, Path)),
    responses(
        (status = 302, description = "Signed URL (Cache-Control max-age=3600)", headers(("Location" = String))),
        Unauthorized, NotFound
    )
)]
pub async fn get_asset(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(asset_id): Path<Uuid>,
) -> ApiResult<Response> {
    let row = sqlx::query!(
        r#"SELECT a.object_name, p.status FROM package_assets a JOIN packages p ON p.id = a.package_id
           WHERE a.id = $1 AND p.deleted_at IS NULL"#,
        asset_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)?;
    if row.status != "published" && !user.is_admin() {
        return Err(ApiError::not_found());
    }
    let signed = state
        .storage
        .sign_get(BucketKind::Assets, &row.object_name, REDIRECT_TTL)
        .await?;
    let mut resp = StatusCode::FOUND.into_response();
    let h = resp.headers_mut();
    h.insert(
        header::LOCATION,
        HeaderValue::from_str(&signed.url).map_err(ApiError::internal_from)?,
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=3600"),
    );
    Ok(resp)
}
