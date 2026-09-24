//! Packages and the public catalog (A1-T09).
//!
//! Admins create and edit packages; the catalog shows only packages that are
//! `published`, not deleted, and have at least one release.

use std::collections::{BTreeMap, HashMap};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use unicode_normalization::UnicodeNormalization;
use utoipa::IntoParams;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_proto::{
    FieldError,
    auth::UserPublic,
    jobs::{Job, JobState},
    packages::{
        AdminPackage, AdminPackageCreate, AdminPackagePage, AdminPackagePatch, Asset, AssetKind,
        AssetSource, FieldSource, ImageType, PackageDetail, PackagePage, PackageStatus,
        PackageSummary, Platform, ProtonDbTier, ReleaseInfo,
    },
};

use crate::{
    audit,
    auth::{CurrentUser, RequestMeta, RequireAdmin},
    error::{ApiError, ApiResult},
    http::{
        etag::{IfMatch, etag, etag_header},
        idempotency::{self, IdempotencyKey},
        json::{Json, Validate, invalid},
        pagination::{CursorCodec, PageParams, finish_page},
        query::Query,
    },
    jobs::{self, Enqueue},
    openapi_problems::{
        BadRequest, Conflict, Forbidden, NotFound, PreconditionFailed, PreconditionRequired,
        Unauthorized,
    },
    state::AppState,
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_catalog))
        .routes(routes!(get_package))
        .routes(routes!(admin_list, admin_create))
        .routes(routes!(admin_get, admin_update, admin_delete))
}

// ---------------------------------------------------------------------------------------
// Enum mapping (text columns ↔ proto enums)
// ---------------------------------------------------------------------------------------

pub fn status_str(s: PackageStatus) -> &'static str {
    match s {
        PackageStatus::Draft => "draft",
        PackageStatus::Published => "published",
        PackageStatus::Hidden => "hidden",
        PackageStatus::Archived => "archived",
    }
}

fn parse_status(s: &str) -> PackageStatus {
    match s {
        "published" => PackageStatus::Published,
        "hidden" => PackageStatus::Hidden,
        "archived" => PackageStatus::Archived,
        _ => PackageStatus::Draft,
    }
}

pub fn asset_kind_str(k: AssetKind) -> &'static str {
    match k {
        AssetKind::Cover => "cover",
        AssetKind::Hero => "hero",
        AssetKind::Logo => "logo",
        AssetKind::Screenshot => "screenshot",
        AssetKind::Icon => "icon",
    }
}

pub fn parse_asset_kind(s: &str) -> Option<AssetKind> {
    Some(match s {
        "cover" => AssetKind::Cover,
        "hero" => AssetKind::Hero,
        "logo" => AssetKind::Logo,
        "screenshot" => AssetKind::Screenshot,
        "icon" => AssetKind::Icon,
        _ => return None,
    })
}

fn parse_image_type(s: &str) -> ImageType {
    match s {
        "image/png" => ImageType::Png,
        "image/webp" => ImageType::Webp,
        _ => ImageType::Jpeg,
    }
}

fn parse_asset_source(s: &str) -> AssetSource {
    match s {
        "igdb" => AssetSource::Igdb,
        "steam" => AssetSource::Steam,
        _ => AssetSource::Upload,
    }
}

fn parse_tier(s: &str) -> Option<ProtonDbTier> {
    Some(match s {
        "platinum" => ProtonDbTier::Platinum,
        "gold" => ProtonDbTier::Gold,
        "silver" => ProtonDbTier::Silver,
        "bronze" => ProtonDbTier::Bronze,
        "borked" => ProtonDbTier::Borked,
        "pending" => ProtonDbTier::Pending,
        _ => return None,
    })
}

fn parse_job_state(s: &str) -> JobState {
    match s {
        "running" => JobState::Running,
        "succeeded" => JobState::Succeeded,
        "failed" => JobState::Failed,
        "dead" => JobState::Dead,
        _ => JobState::Queued,
    }
}

// ---------------------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------------------

struct PackageRow {
    id: Uuid,
    slug: String,
    title: String,
    summary: Option<String>,
    description: Option<String>,
    developer: Option<String>,
    publisher: Option<String>,
    release_date: Option<time::Date>,
    genres: Vec<String>,
    steam_app_id: Option<i64>,
    igdb_id: Option<i64>,
    cover_asset_id: Option<Uuid>,
    hero_asset_id: Option<Uuid>,
    logo_asset_id: Option<Uuid>,
    field_sources: Value,
    protondb_tier: Option<String>,
    status: String,
    created_by: Uuid,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

async fn fetch_rows(state: &AppState, ids: &[Uuid]) -> ApiResult<Vec<PackageRow>> {
    let mut rows = sqlx::query_as!(
        PackageRow,
        r#"SELECT id, slug, title, summary, description, developer, publisher, release_date, genres,
                  steam_app_id, igdb_id, cover_asset_id, hero_asset_id, logo_asset_id, field_sources,
                  protondb_tier, status, created_by, created_at, updated_at
           FROM packages WHERE id = ANY($1) AND deleted_at IS NULL"#,
        ids
    )
    .fetch_all(&state.db)
    .await?;
    let order: HashMap<Uuid, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    rows.sort_by_key(|r| order.get(&r.id).copied().unwrap_or(usize::MAX));
    Ok(rows)
}

fn asset_dto(id: Uuid, kind: &str, w: i32, h: i32, ct: &str, source: &str) -> Asset {
    Asset {
        id,
        kind: parse_asset_kind(kind).unwrap_or(AssetKind::Screenshot),
        url: format!("/v1/assets/{id}"),
        width: w,
        height: h,
        content_type: parse_image_type(ct),
        source: Some(parse_asset_source(source)),
    }
}

/// Assets per package.
async fn assets_for(state: &AppState, ids: &[Uuid]) -> ApiResult<HashMap<Uuid, Vec<Asset>>> {
    let rows = sqlx::query!(
        r#"SELECT id, package_id, kind, width, height, content_type, source FROM package_assets
           WHERE package_id = ANY($1) ORDER BY kind, position, created_at"#,
        ids
    )
    .fetch_all(&state.db)
    .await?;
    let mut out: HashMap<Uuid, Vec<Asset>> = HashMap::new();
    for r in rows {
        out.entry(r.package_id).or_default().push(asset_dto(
            r.id,
            &r.kind,
            r.width,
            r.height,
            &r.content_type,
            &r.source,
        ));
    }
    Ok(out)
}

async fn releases_for(
    state: &AppState,
    ids: &[Uuid],
) -> ApiResult<HashMap<Uuid, Vec<ReleaseInfo>>> {
    let rows = sqlx::query!(
        r#"SELECT r.package_id, r.platform, v.id AS version_id, v.version_label, v.sequence,
                  v.total_size AS "total_size!", v.published_at AS "published_at!"
           FROM package_releases r JOIN package_versions v ON v.id = r.version_id
           WHERE r.package_id = ANY($1) AND v.total_size IS NOT NULL AND v.published_at IS NOT NULL
           ORDER BY r.platform"#,
        ids
    )
    .fetch_all(&state.db)
    .await?;
    let mut out: HashMap<Uuid, Vec<ReleaseInfo>> = HashMap::new();
    for r in rows {
        let Some(platform) = Platform::parse(&r.platform) else {
            continue;
        };
        out.entry(r.package_id).or_default().push(ReleaseInfo {
            platform,
            version_id: r.version_id,
            version_label: r.version_label,
            sequence: r.sequence,
            total_size: r.total_size,
            published_at: r.published_at,
        });
    }
    Ok(out)
}

fn summary(row: &PackageRow, assets: &[Asset], releases: &[ReleaseInfo]) -> PackageSummary {
    PackageSummary {
        id: row.id,
        slug: row.slug.clone(),
        title: row.title.clone(),
        summary: row.summary.clone(),
        genres: row.genres.clone(),
        cover: row
            .cover_asset_id
            .and_then(|id| assets.iter().find(|a| a.id == id).cloned()),
        platforms: releases.iter().map(|r| r.platform).collect(),
        updated_at: row.updated_at,
    }
}

fn detail(row: &PackageRow, assets: &[Asset], releases: &[ReleaseInfo]) -> PackageDetail {
    let find = |id: Option<Uuid>| id.and_then(|id| assets.iter().find(|a| a.id == id).cloned());
    PackageDetail {
        summary: summary(row, assets, releases),
        description: row.description.clone(),
        developer: row.developer.clone(),
        publisher: row.publisher.clone(),
        release_date: row.release_date,
        protondb_tier: row.protondb_tier.as_deref().and_then(parse_tier),
        hero: find(row.hero_asset_id),
        logo: find(row.logo_asset_id),
        screenshots: assets
            .iter()
            .filter(|a| a.kind == AssetKind::Screenshot)
            .cloned()
            .collect(),
        releases: releases.to_vec(),
    }
}

pub async fn users_public(state: &AppState, ids: &[Uuid]) -> ApiResult<HashMap<Uuid, UserPublic>> {
    let rows = sqlx::query!(
        "SELECT id, discord_id, username, display_name, avatar_hash FROM users WHERE id = ANY($1)",
        ids
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.id,
                UserPublic {
                    id: r.id,
                    username: r.username,
                    display_name: r.display_name,
                    avatar_url: crate::auth::discord::avatar_url(
                        &r.discord_id,
                        r.avatar_hash.as_deref(),
                    ),
                },
            )
        })
        .collect())
}

/// The latest `metadata.fetch` job per package.
async fn metadata_jobs(state: &AppState, ids: &[Uuid]) -> ApiResult<HashMap<Uuid, Job>> {
    let keys: Vec<String> = ids.iter().map(Uuid::to_string).collect();
    let rows = sqlx::query!(
        r#"SELECT DISTINCT ON (payload->>'package_id') payload->>'package_id' AS "package_id!",
                  id, kind, state, attempts, max_attempts, last_error, run_at, created_at, finished_at
           FROM jobs
           WHERE kind = 'metadata.fetch' AND payload->>'package_id' = ANY($1)
           ORDER BY payload->>'package_id', created_at DESC"#,
        &keys
    )
    .fetch_all(&state.db)
    .await?;
    let mut out = HashMap::new();
    for r in rows {
        if let Ok(pid) = Uuid::parse_str(&r.package_id) {
            out.insert(
                pid,
                Job {
                    id: r.id,
                    kind: r.kind,
                    state: parse_job_state(&r.state),
                    attempts: r.attempts,
                    max_attempts: r.max_attempts,
                    last_error: r.last_error,
                    run_at: Some(r.run_at),
                    created_at: r.created_at,
                    finished_at: r.finished_at,
                },
            );
        }
    }
    Ok(out)
}

fn field_sources(v: &Value) -> BTreeMap<String, FieldSource> {
    v.as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| {
                    let s = match v.as_str()? {
                        "admin" => FieldSource::Admin,
                        "igdb" => FieldSource::Igdb,
                        "steam" => FieldSource::Steam,
                        _ => return None,
                    };
                    Some((k.clone(), s))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Full admin views, in the order of `ids` (deleted packages are skipped).
pub async fn load_admin(
    state: &AppState,
    ids: &[Uuid],
) -> ApiResult<Vec<(AdminPackage, OffsetDateTime)>> {
    let rows = fetch_rows(state, ids).await?;
    let assets = assets_for(state, ids).await?;
    let releases = releases_for(state, ids).await?;
    let creators: Vec<Uuid> = rows.iter().map(|r| r.created_by).collect();
    let users = users_public(state, &creators).await?;
    let jobs = metadata_jobs(state, ids).await?;
    Ok(rows
        .iter()
        .map(|r| {
            let a = assets.get(&r.id).map_or(&[][..], Vec::as_slice);
            let rel = releases.get(&r.id).map_or(&[][..], Vec::as_slice);
            let pkg = AdminPackage {
                detail: detail(r, a, rel),
                status: parse_status(&r.status),
                steam_app_id: r.steam_app_id,
                igdb_id: r.igdb_id,
                field_sources: field_sources(&r.field_sources),
                created_at: r.created_at,
                created_by: users.get(&r.created_by).cloned().unwrap_or(UserPublic {
                    id: r.created_by,
                    username: "unknown".into(),
                    display_name: None,
                    avatar_url: None,
                }),
                metadata_job: jobs.get(&r.id).cloned(),
            };
            (pkg, r.updated_at)
        })
        .collect())
}

pub async fn load_one_admin(
    state: &AppState,
    id: Uuid,
) -> ApiResult<(AdminPackage, OffsetDateTime)> {
    load_admin(state, &[id])
        .await?
        .into_iter()
        .next()
        .ok_or_else(ApiError::not_found)
}

// ---------------------------------------------------------------------------------------
// Slugs and validation
// ---------------------------------------------------------------------------------------

pub fn valid_slug(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && b.first().is_some_and(u8::is_ascii_alphanumeric)
        && b.last().is_some_and(u8::is_ascii_alphanumeric)
}

/// ASCII slug from a title: accents stripped (NFKD), non-alphanumerics become `-`.
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in title.nfkd() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if unicode_normalization::char::is_combining_mark(c) {
            continue;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    let s: String = out.trim_matches('-').chars().take(56).collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "package".to_string()
    } else {
        s
    }
}

async fn unique_slug(state: &AppState, base: &str) -> ApiResult<String> {
    let taken: Vec<String> = sqlx::query_scalar!(
        r#"SELECT slug FROM packages WHERE slug = $1 OR slug LIKE $2 ESCAPE '\'"#,
        base,
        format!(
            "{}-%",
            base.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    )
    .fetch_all(&state.db)
    .await?;
    if !taken.iter().any(|s| s == base) {
        return Ok(base.to_string());
    }
    for n in 2..10_000 {
        let candidate = format!("{base}-{n}");
        if !taken.contains(&candidate) {
            return Ok(candidate);
        }
    }
    Err(ApiError::conflict(
        "slug_taken",
        "No free slug is available for this title",
    ))
}

fn check_text(errors: &mut Vec<FieldError>, field: &str, v: &str, min: usize, max: usize) {
    let n = v.trim().chars().count();
    if n < min || n > max {
        invalid(
            errors,
            field,
            "length",
            format!("must be {min}-{max} characters"),
        );
    }
}

impl Validate for AdminPackageCreate {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        check_text(errors, "title", &self.title, 1, 200);
        if let Some(s) = &self.slug
            && !valid_slug(s)
        {
            invalid(
                errors,
                "slug",
                "pattern",
                "must be lowercase letters, digits and dashes (1-64)",
            );
        }
        if self.steam_app_id.is_some_and(|v| v < 1) {
            invalid(errors, "steam_app_id", "minimum", "must be at least 1");
        }
        if self.igdb_id.is_some_and(|v| v < 1) {
            invalid(errors, "igdb_id", "minimum", "must be at least 1");
        }
    }
}

impl Validate for AdminPackagePatch {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if let Some(t) = &self.title {
            check_text(errors, "title", t, 1, 200);
        }
        if let Some(s) = &self.slug
            && !valid_slug(s)
        {
            invalid(
                errors,
                "slug",
                "pattern",
                "must be lowercase letters, digits and dashes (1-64)",
            );
        }
        for (field, value, max) in [
            ("summary", &self.summary, 500),
            ("description", &self.description, 20_000),
            ("developer", &self.developer, 200),
            ("publisher", &self.publisher, 200),
        ] {
            if let Some(Some(v)) = value
                && v.chars().count() > max
            {
                invalid(
                    errors,
                    field,
                    "max_length",
                    format!("must be at most {max} characters"),
                );
            }
        }
        if let Some(g) = &self.genres {
            if g.len() > 20 {
                invalid(errors, "genres", "max_items", "at most 20 genres");
            }
            for (i, v) in g.iter().enumerate() {
                let n = v.trim().chars().count();
                if !(1..=64).contains(&n) {
                    invalid(
                        errors,
                        &format!("genres[{i}]"),
                        "length",
                        "must be 1-64 characters",
                    );
                }
            }
        }
        if matches!(self.steam_app_id, Some(Some(v)) if v < 1) {
            invalid(errors, "steam_app_id", "minimum", "must be at least 1");
        }
        if matches!(self.igdb_id, Some(Some(v)) if v < 1) {
            invalid(errors, "igdb_id", "minimum", "must be at least 1");
        }
    }
}

// ---------------------------------------------------------------------------------------
// Public catalog
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSort {
    #[default]
    Title,
    Recent,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct CatalogQuery {
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    /// Case-insensitive title search
    pub q: Option<String>,
    pub genre: Option<String>,
    /// Only packages with a release for this platform
    #[param(inline)]
    pub platform: Option<Platform>,
    #[param(inline)]
    pub sort: Option<CatalogSort>,
}

impl Validate for CatalogQuery {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        PageParams {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
        .validate(errors);
        if self.q.as_ref().is_some_and(|q| q.chars().count() > 100) {
            invalid(errors, "q", "max_length", "must be at most 100 characters");
        }
        if self.genre.as_ref().is_some_and(|g| g.chars().count() > 64) {
            invalid(
                errors,
                "genre",
                "max_length",
                "must be at most 64 characters",
            );
        }
    }
}

fn like_pattern(q: &str) -> String {
    let escaped = q
        .trim()
        .to_lowercase()
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// Browse published packages
#[utoipa::path(
    get,
    path = "/v1/packages",
    tag = "catalog",
    operation_id = "listPackages",
    params(CatalogQuery),
    responses((status = 200, description = "Page of packages", body = PackagePage), BadRequest, Unauthorized)
)]
pub async fn list_catalog(
    State(state): State<AppState>,
    _user: CurrentUser,
    Query(q): Query<CatalogQuery>,
) -> ApiResult<axum::Json<PackagePage>> {
    let limit = q.limit.unwrap_or(crate::http::pagination::DEFAULT_LIMIT);
    let sort = q.sort.unwrap_or_default();
    let filters = json!({ "q": q.q, "genre": q.genre, "platform": q.platform, "sort": sort });
    let codec = CursorCodec::new(state.keys.cursor.expose());
    let after: Option<(String, Uuid)> = q
        .cursor
        .as_deref()
        .map(|c| codec.decode(c, &filters))
        .transpose()?;
    let pattern =
        q.q.as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(like_pattern);
    let platform = q.platform.map(|p| p.as_str().to_string());

    let rows = sqlx::query!(
        r#"SELECT p.id, lower(p.title) AS "title_key!", p.updated_at
           FROM packages p
           WHERE p.deleted_at IS NULL AND p.status = 'published'
             AND EXISTS (SELECT 1 FROM package_releases r WHERE r.package_id = p.id
                         AND ($4::text IS NULL OR r.platform = $4))
             AND ($1::text IS NULL OR lower(p.title) LIKE $1 ESCAPE '\')
             AND ($2::text IS NULL OR $2 = ANY(p.genres))
             AND ($3::text IS NULL OR
                  CASE WHEN $5 = 'title' THEN (lower(p.title), p.id) > ($3, $6)
                       ELSE (p.updated_at, p.id) < ($3::timestamptz, $6) END)
           ORDER BY
             CASE WHEN $5 = 'title' THEN lower(p.title) END ASC,
             CASE WHEN $5 = 'recent' THEN p.updated_at END DESC,
             p.id
           LIMIT $7"#,
        pattern,
        q.genre,
        after.as_ref().map(|(k, _)| k.clone()),
        platform,
        if sort == CatalogSort::Title {
            "title"
        } else {
            "recent"
        },
        after.map(|(_, id)| id).unwrap_or(Uuid::nil()),
        i64::from(limit) + 1
    )
    .fetch_all(&state.db)
    .await?;

    let keyed: Vec<(Uuid, String, OffsetDateTime)> = rows
        .into_iter()
        .map(|r| (r.id, r.title_key, r.updated_at))
        .collect();
    let sort_key = |r: &(Uuid, String, OffsetDateTime)| -> String {
        if sort == CatalogSort::Title {
            r.1.clone()
        } else {
            r.2.format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default()
        }
    };
    let page = finish_page(keyed, limit, |r| codec.encode(&sort_key(r), r.0, &filters))?;
    let ids: Vec<Uuid> = page.items.iter().map(|r| r.0).collect();
    let rows = fetch_rows(&state, &ids).await?;
    let assets = assets_for(&state, &ids).await?;
    let releases = releases_for(&state, &ids).await?;
    let items = rows
        .iter()
        .map(|r| {
            summary(
                r,
                assets.get(&r.id).map_or(&[][..], Vec::as_slice),
                releases.get(&r.id).map_or(&[][..], Vec::as_slice),
            )
        })
        .collect();
    Ok(axum::Json(PackagePage {
        items,
        next_cursor: page.next_cursor,
    }))
}

/// Package details
#[utoipa::path(
    get,
    path = "/v1/packages/{package_id}",
    tag = "catalog",
    operation_id = "getPackage",
    params(("package_id" = Uuid, Path)),
    responses((status = 200, description = "Package", body = PackageDetail), Unauthorized, NotFound)
)]
pub async fn get_package(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<axum::Json<PackageDetail>> {
    let (pkg, _) = load_one_admin(&state, id).await?;
    // Admins may preview unpublished packages; everyone else sees only the public catalog.
    if !user.is_admin()
        && (pkg.status != PackageStatus::Published || pkg.detail.releases.is_empty())
    {
        return Err(ApiError::not_found());
    }
    Ok(axum::Json(pkg.detail))
}

// ---------------------------------------------------------------------------------------
// Admin
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct AdminListQuery {
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    pub q: Option<String>,
    #[param(inline)]
    pub status: Option<PackageStatus>,
}

impl Validate for AdminListQuery {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        PageParams {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
        .validate(errors);
        if self.q.as_ref().is_some_and(|q| q.chars().count() > 100) {
            invalid(errors, "q", "max_length", "must be at most 100 characters");
        }
    }
}

/// All packages, any status
#[utoipa::path(
    get,
    path = "/v1/admin/packages",
    tag = "admin-packages",
    operation_id = "adminListPackages",
    params(AdminListQuery),
    responses((status = 200, description = "Page", body = AdminPackagePage), Unauthorized, Forbidden)
)]
pub async fn admin_list(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Query(q): Query<AdminListQuery>,
) -> ApiResult<axum::Json<AdminPackagePage>> {
    let limit = q.limit.unwrap_or(crate::http::pagination::DEFAULT_LIMIT);
    let filters = json!({ "q": q.q, "status": q.status });
    let codec = CursorCodec::new(state.keys.cursor.expose());
    let after: Option<(String, Uuid)> = q
        .cursor
        .as_deref()
        .map(|c| codec.decode(c, &filters))
        .transpose()?;
    let rows = sqlx::query!(
        r#"SELECT id, lower(title) AS "title_key!" FROM packages
           WHERE deleted_at IS NULL
             AND ($1::text IS NULL OR lower(title) LIKE $1 ESCAPE '\' OR slug LIKE $1 ESCAPE '\')
             AND ($2::text IS NULL OR status = $2)
             AND ($3::text IS NULL OR (lower(title), id) > ($3, $4))
           ORDER BY lower(title), id
           LIMIT $5"#,
        q.q.as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(like_pattern),
        q.status.map(status_str),
        after.as_ref().map(|(k, _)| k.clone()),
        after.map(|(_, id)| id).unwrap_or(Uuid::nil()),
        i64::from(limit) + 1
    )
    .fetch_all(&state.db)
    .await?;
    let keyed: Vec<(Uuid, String)> = rows.into_iter().map(|r| (r.id, r.title_key)).collect();
    let page = finish_page(keyed, limit, |r| codec.encode(&r.1, r.0, &filters))?;
    let ids: Vec<Uuid> = page.items.iter().map(|r| r.0).collect();
    let items = load_admin(&state, &ids)
        .await?
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    Ok(axum::Json(AdminPackagePage {
        items,
        next_cursor: page.next_cursor,
    }))
}

fn with_etag(mut resp: Response, updated_at: OffsetDateTime) -> Response {
    let (k, v) = etag_header(updated_at);
    resp.headers_mut().insert(k, v);
    resp
}

/// Create a package (enqueues metadata fetch)
#[utoipa::path(
    post,
    path = "/v1/admin/packages",
    tag = "admin-packages",
    operation_id = "adminCreatePackage",
    params(("Idempotency-Key" = Option<String>, Header, pattern = "^[A-Za-z0-9_-]{16,128}$")),
    request_body = AdminPackageCreate,
    responses(
        (status = 201, description = "Created", body = AdminPackage, headers(("ETag" = String))),
        BadRequest, Unauthorized, Forbidden, Conflict
    )
)]
pub async fn admin_create(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    key: IdempotencyKey,
    Json(req): Json<AdminPackageCreate>,
) -> ApiResult<Response> {
    let fp = idempotency::fingerprint("POST", "/v1/admin/packages", &req);
    let st = state.clone();
    let mut resp = idempotency::run(&state.db, admin.user_id, &key, fp, || async move {
        let title = req.title.trim().to_string();
        let slug = match &req.slug {
            Some(s) => {
                let taken = sqlx::query_scalar!(
                    r#"SELECT EXISTS (SELECT 1 FROM packages WHERE slug = $1) AS "e!""#,
                    s
                )
                .fetch_one(&st.db)
                .await?;
                if taken {
                    return Err(ApiError::conflict(
                        "slug_taken",
                        "Another package already uses this slug",
                    ));
                }
                s.clone()
            }
            None => unique_slug(&st, &slugify(&title)).await?,
        };
        let mut sources = serde_json::Map::new();
        sources.insert("title".into(), json!("admin"));
        if req.steam_app_id.is_some() {
            sources.insert("steam_app_id".into(), json!("admin"));
        }
        if req.igdb_id.is_some() {
            sources.insert("igdb_id".into(), json!("admin"));
        }
        let mut tx = st.db.begin().await?;
        let id = sqlx::query_scalar!(
            r#"INSERT INTO packages (slug, title, steam_app_id, igdb_id, field_sources, created_by)
               VALUES ($1, $2, $3, $4, $5, $6) RETURNING id"#,
            slug,
            title,
            req.steam_app_id,
            req.igdb_id,
            Value::Object(sources),
            admin.user_id
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| {
            if crate::error::is_unique_violation(&e, None) {
                ApiError::conflict("slug_taken", "Another package already uses this slug")
            } else {
                e.into()
            }
        })?;
        if req.fetch_metadata.unwrap_or(true) {
            jobs::enqueue(
                &mut tx,
                "metadata.fetch",
                json!({ "package_id": id.to_string() }),
                Enqueue {
                    dedupe_key: Some(format!("metadata.fetch:{id}")),
                    ..Default::default()
                },
            )
            .await?;
        }
        audit::record(
            &mut tx,
            admin.user_id,
            &meta,
            "package.create",
            "package",
            &id.to_string(),
            json!({ "slug": slug, "title": title }),
        )
        .await?;
        tx.commit().await?;
        let (pkg, _) = load_one_admin(&st, id).await?;
        Ok((
            StatusCode::CREATED,
            serde_json::to_value(pkg).map_err(ApiError::internal_from)?,
        ))
    })
    .await?;
    // The ETag always reflects the current row, also on replays.
    if let Some(id) = resp
        .body
        .get("id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        && let Some(updated) =
            sqlx::query_scalar!("SELECT updated_at FROM packages WHERE id = $1", id)
                .fetch_optional(&state.db)
                .await?
    {
        resp.headers.push(etag_header(updated));
    }
    Ok(resp.into_response())
}

/// Package with admin fields
#[utoipa::path(
    get,
    path = "/v1/admin/packages/{package_id}",
    tag = "admin-packages",
    operation_id = "adminGetPackage",
    params(("package_id" = Uuid, Path)),
    responses(
        (status = 200, description = "Package", body = AdminPackage, headers(("ETag" = String))),
        Unauthorized, Forbidden, NotFound
    )
)]
pub async fn admin_get(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let (pkg, updated) = load_one_admin(&state, id).await?;
    Ok(with_etag(axum::Json(pkg).into_response(), updated))
}

/// Edit package fields (JSON Merge Patch, requires If-Match)
#[utoipa::path(
    patch,
    path = "/v1/admin/packages/{package_id}",
    tag = "admin-packages",
    operation_id = "adminUpdatePackage",
    params(("package_id" = Uuid, Path), ("If-Match" = String, Header)),
    request_body(content = AdminPackagePatch, content_type = "application/merge-patch+json"),
    responses(
        (status = 200, description = "Updated", body = AdminPackage, headers(("ETag" = String))),
        BadRequest, Unauthorized, Forbidden, NotFound, Conflict, PreconditionFailed, PreconditionRequired
    )
)]
pub async fn admin_update(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Path(id): Path<Uuid>,
    if_match: IfMatch,
    Json(patch): Json<AdminPackagePatch>,
) -> ApiResult<Response> {
    let mut tx = state.db.begin().await?;
    let current = sqlx::query!(
        "SELECT updated_at FROM packages WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    if_match.check(&etag(current.updated_at))?;

    // Asset references must belong to this package and be of the matching kind.
    for (field, value, kind) in [
        ("cover_asset_id", patch.cover_asset_id, "cover"),
        ("hero_asset_id", patch.hero_asset_id, "hero"),
        ("logo_asset_id", patch.logo_asset_id, "logo"),
    ] {
        if let Some(Some(asset)) = value {
            let ok = sqlx::query_scalar!(
                r#"SELECT EXISTS (SELECT 1 FROM package_assets WHERE id = $1 AND package_id = $2 AND kind = $3) AS "e!""#,
                asset,
                id,
                kind
            )
            .fetch_one(&mut *tx)
            .await?;
            if !ok {
                return Err(ApiError::field(
                    field,
                    "invalid_asset",
                    format!("must be a {kind} image of this package"),
                ));
            }
        }
    }
    if let Some(slug) = &patch.slug {
        let taken = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM packages WHERE slug = $1 AND id <> $2) AS "e!""#,
            slug,
            id
        )
        .fetch_one(&mut *tx)
        .await?;
        if taken {
            return Err(ApiError::conflict(
                "slug_taken",
                "Another package already uses this slug",
            ));
        }
    }

    let changed: Vec<&str> = [
        ("title", patch.title.is_some()),
        ("slug", patch.slug.is_some()),
        ("summary", patch.summary.is_some()),
        ("description", patch.description.is_some()),
        ("developer", patch.developer.is_some()),
        ("publisher", patch.publisher.is_some()),
        ("release_date", patch.release_date.is_some()),
        ("genres", patch.genres.is_some()),
        ("steam_app_id", patch.steam_app_id.is_some()),
        ("igdb_id", patch.igdb_id.is_some()),
        ("status", patch.status.is_some()),
        ("cover", patch.cover_asset_id.is_some()),
        ("hero", patch.hero_asset_id.is_some()),
        ("logo", patch.logo_asset_id.is_some()),
    ]
    .into_iter()
    .filter_map(|(k, set)| set.then_some(k))
    .collect();
    let sources: serde_json::Map<String, Value> = changed
        .iter()
        .filter(|k| **k != "status" && **k != "slug")
        .map(|k| ((*k).to_string(), json!("admin")))
        .collect();
    let genres = patch.genres.as_ref().map(|g| {
        let mut seen = std::collections::HashSet::new();
        g.iter()
            .map(|s| s.trim().to_string())
            .filter(|s| seen.insert(s.clone()))
            .collect::<Vec<_>>()
    });
    let trim = |o: &Option<Option<String>>| {
        o.as_ref().map(|v| {
            v.as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
    };
    let (summary, description, developer, publisher) = (
        trim(&patch.summary),
        trim(&patch.description),
        trim(&patch.developer),
        trim(&patch.publisher),
    );

    sqlx::query!(
        r#"UPDATE packages SET
             title = COALESCE($2, title),
             slug = COALESCE($3, slug),
             summary = CASE WHEN $4 THEN $5 ELSE summary END,
             description = CASE WHEN $6 THEN $7 ELSE description END,
             developer = CASE WHEN $8 THEN $9 ELSE developer END,
             publisher = CASE WHEN $10 THEN $11 ELSE publisher END,
             release_date = CASE WHEN $12 THEN $13 ELSE release_date END,
             genres = COALESCE($14, genres),
             steam_app_id = CASE WHEN $15 THEN $16 ELSE steam_app_id END,
             igdb_id = CASE WHEN $17 THEN $18 ELSE igdb_id END,
             status = COALESCE($19, status),
             cover_asset_id = CASE WHEN $20 THEN $21 ELSE cover_asset_id END,
             hero_asset_id = CASE WHEN $22 THEN $23 ELSE hero_asset_id END,
             logo_asset_id = CASE WHEN $24 THEN $25 ELSE logo_asset_id END,
             field_sources = field_sources || $26
           WHERE id = $1"#,
        id,
        patch.title.as_deref().map(str::trim),
        patch.slug,
        summary.is_some(),
        summary.flatten(),
        description.is_some(),
        description.flatten(),
        developer.is_some(),
        developer.flatten(),
        publisher.is_some(),
        publisher.flatten(),
        patch.release_date.is_some(),
        patch.release_date.flatten(),
        genres.as_deref(),
        patch.steam_app_id.is_some(),
        patch.steam_app_id.flatten(),
        patch.igdb_id.is_some(),
        patch.igdb_id.flatten(),
        patch.status.map(status_str),
        patch.cover_asset_id.is_some(),
        patch.cover_asset_id.flatten(),
        patch.hero_asset_id.is_some(),
        patch.hero_asset_id.flatten(),
        patch.logo_asset_id.is_some(),
        patch.logo_asset_id.flatten(),
        Value::Object(sources)
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        if crate::error::is_unique_violation(&e, None) {
            ApiError::conflict("slug_taken", "Another package already uses this slug")
        } else {
            e.into()
        }
    })?;
    audit::record(
        &mut tx,
        admin.user_id,
        &meta,
        "package.update",
        "package",
        &id.to_string(),
        json!({ "fields": changed }),
    )
    .await?;
    tx.commit().await?;
    let (pkg, updated) = load_one_admin(&state, id).await?;
    Ok(with_etag(axum::Json(pkg).into_response(), updated))
}

/// Soft-delete a package (hidden from clients; installs keep working offline)
#[utoipa::path(
    delete,
    path = "/v1/admin/packages/{package_id}",
    tag = "admin-packages",
    operation_id = "adminDeletePackage",
    params(("package_id" = Uuid, Path), ("If-Match" = String, Header)),
    responses((status = 204, description = "Deleted"), Unauthorized, Forbidden, NotFound, PreconditionFailed)
)]
pub async fn admin_delete(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Path(id): Path<Uuid>,
    if_match: IfMatch,
) -> ApiResult<StatusCode> {
    let mut tx = state.db.begin().await?;
    let current = sqlx::query!(
        "SELECT updated_at FROM packages WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    if_match.check(&etag(current.updated_at))?;
    sqlx::query!("UPDATE packages SET deleted_at = now() WHERE id = $1", id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut tx,
        admin.user_id,
        &meta,
        "package.delete",
        "package",
        &id.to_string(),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("Half-Life 2"), "half-life-2");
        assert_eq!(
            slugify("  Pokémon: Épée & Bouclier!  "),
            "pokemon-epee-bouclier"
        );
        assert_eq!(slugify("東京"), "package");
        assert_eq!(slugify("---"), "package");
        assert!(slugify(&"a very long title ".repeat(10)).len() <= 56);
        for s in ["half-life-2", "a", "x1"] {
            assert!(valid_slug(s), "{s}");
        }
        for s in ["", "-a", "a-", "A", "a_b", &"a".repeat(65)] {
            assert!(!valid_slug(s), "{s}");
        }
    }
}
