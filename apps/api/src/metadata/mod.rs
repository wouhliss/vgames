//! IGDB / Steam metadata fetching (A1-T10, docs/architecture/03-api.md §5).
//!
//! `metadata.fetch` stores candidates and auto-applies only unambiguous matches;
//! `metadata.images` downloads chosen images through [`safe_fetch`].

pub mod normalize;
pub mod providers;
pub mod safe_fetch;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::PgConnection;
use time::{Date, format_description::well_known::Iso8601};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_proto::{
    common::FieldError,
    jobs::Job,
    packages::{
        AdminPackage, AssetKind, AssetSource, CandidateData, MetadataApply, MetadataCandidate,
        MetadataCandidateList, MetadataField, MetadataSource,
    },
};

pub use providers::{Endpoints, Found, ProviderRates, Providers};

use crate::http::path::Path;
use crate::{
    assets, audit,
    auth::{RequestMeta, RequireAdmin},
    error::{ApiError, ApiResult},
    http::{
        etag::{IfMatch, etag},
        json::{Json, Validate, invalid},
    },
    jobs::{self, Enqueue, JobContext, JobError, JobHandler, handler},
    openapi_problems::{
        BadRequest, Forbidden, NotFound, PreconditionFailed, PreconditionRequired, Unauthorized,
    },
    packages,
    state::AppState,
};

/// Titles at least this similar count as a match for auto-apply.
pub const AUTO_APPLY_SCORE: f32 = 0.95;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(refresh))
        .routes(routes!(list_candidates))
        .routes(routes!(apply))
}

pub fn job_handlers() -> Vec<(&'static str, JobHandler)> {
    vec![
        ("metadata.fetch", handler(fetch_job)),
        ("metadata.images", handler(images_job)),
    ]
}

/// Queues a metadata fetch unless one is already queued or running for the package.
pub async fn enqueue_fetch(
    conn: &mut PgConnection,
    package_id: Uuid,
) -> Result<Option<Uuid>, ApiError> {
    jobs::enqueue(
        conn,
        "metadata.fetch",
        json!({ "package_id": package_id.to_string() }),
        Enqueue {
            dedupe_key: Some(format!("metadata.fetch:{package_id}")),
            ..Default::default()
        },
    )
    .await
}

// ---------------------------------------------------------------------------------------
// metadata.fetch
// ---------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct FetchPayload {
    package_id: Uuid,
}

async fn fetch_job(ctx: JobContext) -> Result<(), JobError> {
    let p: FetchPayload = serde_json::from_value(ctx.payload.clone())
        .map_err(|e| JobError::Fatal(format!("invalid payload: {e}")))?;
    let state = &ctx.state;
    let Some(pkg) = sqlx::query!(
        "SELECT title, steam_app_id, igdb_id FROM packages WHERE id = $1 AND deleted_at IS NULL",
        p.package_id
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(());
    };
    let prov = &state.metadata;

    let mut found: Vec<Found> = Vec::new();
    if prov.igdb_enabled() {
        found.extend(match (pkg.igdb_id, pkg.steam_app_id) {
            (Some(id), _) => prov.igdb_by_id(id).await?,
            (None, Some(app)) => prov.igdb_by_steam_id(app).await?,
            (None, None) => prov.igdb_search(&pkg.title).await?,
        });
    }
    // An explicit IGDB id also tells us the Steam app.
    let steam_hint = pkg.steam_app_id.or_else(|| {
        pkg.igdb_id.and_then(|id| {
            found
                .iter()
                .find(|f| f.source == MetadataSource::Igdb && f.external_id == id)
                .and_then(|f| f.data.external.steam_app_id)
        })
    });
    if prov.steam_enabled() {
        match steam_hint {
            Some(id) => found.extend(prov.steam_by_id(id).await?),
            None => found.extend(prov.steam_search(&pkg.title).await?),
        }
    }
    let mut seen = std::collections::HashSet::new();
    found.retain(|f| seen.insert((f.source, f.external_id)));

    let mut scored: Vec<(f32, Found)> = found
        .into_iter()
        .map(|f| (f.score(&pkg.title), f))
        .collect();
    let picks = auto_picks(&scored, pkg.igdb_id, steam_hint);

    // Compatibility hints for admins; best effort, never fails the job.
    let steam_id = steam_hint.or_else(|| {
        picks
            .iter()
            .find_map(|&i| scored.get(i)?.1.data.external.steam_app_id)
    });
    let mut umu_id = None;
    let mut tier = None;
    if let (Some(id), true) = (steam_id, prov.steam_enabled()) {
        match prov.umu_id(id).await {
            Ok(u) => umu_id = u,
            Err(e) => tracing::info!(error = %e, "umu-database lookup failed"),
        }
        match prov.protondb_tier(id).await {
            Ok(t) => tier = t,
            Err(e) => tracing::info!(error = %e, "ProtonDB lookup failed"),
        }
    }

    let mut tx = state.db.begin().await?;
    sqlx::query!(
        "DELETE FROM metadata_candidates WHERE package_id = $1",
        p.package_id
    )
    .execute(&mut *tx)
    .await?;
    for (score, f) in &mut scored {
        if f.source == MetadataSource::Steam && Some(f.external_id) == steam_id {
            f.data.external.umu_id.clone_from(&umu_id);
        }
        sqlx::query!(
            r#"INSERT INTO metadata_candidates (package_id, source, external_id, title, release_year, score, data)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
            p.package_id,
            f.source.as_str(),
            f.external_id,
            f.title,
            f.release_year.and_then(|y| i16::try_from(y).ok()),
            *score,
            serde_json::to_value(&f.data)?
        )
        .execute(&mut *tx)
        .await?;
    }
    if let Some(t) = tier {
        sqlx::query!(
            "UPDATE packages SET protondb_tier = $2 WHERE id = $1 AND protondb_tier IS DISTINCT FROM $2",
            p.package_id,
            packages::protondb_tier_str(t)
        )
        .execute(&mut *tx)
        .await?;
    }
    let chosen: Vec<Pick<'_>> = picks
        .iter()
        .filter_map(|&i| scored.get(i))
        .map(|(_, f)| Pick {
            source: f.source,
            external_id: f.external_id,
            data: &f.data,
        })
        .collect();
    if !chosen.is_empty() {
        apply_picks(&mut tx, p.package_id, &chosen, &MetadataField::ALL, false)
            .await
            .map_err(|e| JobError::Retry(e.to_string()))?;
    }
    tx.commit().await?;
    Ok(())
}

/// Indices (into `scored`) of the candidates to apply automatically, IGDB first.
///
/// With explicit ids, the candidates carrying those ids. Otherwise only an unambiguous title
/// match: at most one candidate per provider scores ≥ [`AUTO_APPLY_SCORE`], and when both
/// providers match, the IGDB entry must link to that very Steam app.
pub fn auto_picks(
    scored: &[(f32, Found)],
    igdb_id: Option<i64>,
    steam_id: Option<i64>,
) -> Vec<usize> {
    let by = |source: MetadataSource| {
        scored
            .iter()
            .enumerate()
            .filter(move |(_, (_, f))| f.source == source)
    };
    if igdb_id.is_some() || steam_id.is_some() {
        let igdb = by(MetadataSource::Igdb).find(|(_, (_, f))| match igdb_id {
            Some(id) => f.external_id == id,
            None => f.data.external.steam_app_id == steam_id,
        });
        let steam = by(MetadataSource::Steam).find(|(_, (_, f))| Some(f.external_id) == steam_id);
        return igdb.into_iter().chain(steam).map(|(i, _)| i).collect();
    }
    let strong = |source| {
        by(source)
            .filter(|(_, (s, _))| *s >= AUTO_APPLY_SCORE)
            .collect::<Vec<_>>()
    };
    let (igdb, steam) = (strong(MetadataSource::Igdb), strong(MetadataSource::Steam));
    match (igdb.as_slice(), steam.as_slice()) {
        ([(i, _)], []) | ([], [(i, _)]) => vec![*i],
        ([(i, (_, g))], [(j, (_, s))]) if g.data.external.steam_app_id == Some(s.external_id) => {
            vec![*i, *j]
        }
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------------------
// Applying candidates
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct Pick<'a> {
    pub source: MetadataSource,
    pub external_id: i64,
    pub data: &'a CandidateData,
}

#[derive(Debug, Serialize, Deserialize)]
struct ImagesPayload {
    package_id: Uuid,
    overwrite_admin: bool,
    images: Vec<ImageTask>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ImageTask {
    kind: AssetKind,
    source: MetadataSource,
    url: String,
}

fn first<'a, T>(
    picks: &[Pick<'a>],
    f: impl Fn(&Pick<'a>) -> Option<T>,
) -> Option<(T, MetadataSource)> {
    picks.iter().find_map(|p| f(p).map(|v| (v, p.source)))
}

/// Reads one image URL out of a candidate.
type ImageUrl = fn(&CandidateData) -> Option<String>;

/// Package columns set by [`apply_picks`] (`None` keeps the current value).
#[derive(Default)]
struct Updates {
    title: Option<String>,
    summary: Option<String>,
    description: Option<String>,
    developer: Option<String>,
    publisher: Option<String>,
    release_date: Option<Date>,
    genres: Option<Vec<String>>,
    steam_app_id: Option<i64>,
    igdb_id: Option<i64>,
}

/// Writes the chosen fields (never `admin` ones unless `overwrite_admin`) and queues the
/// image downloads. The earlier pick wins when several provide a field.
async fn apply_picks(
    conn: &mut PgConnection,
    package_id: Uuid,
    picks: &[Pick<'_>],
    fields: &[MetadataField],
    overwrite_admin: bool,
) -> ApiResult<()> {
    let Some(row) = sqlx::query!(
        "SELECT field_sources FROM packages WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
        package_id
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(());
    };
    let mut sources: Map<String, Value> =
        row.field_sources.as_object().cloned().unwrap_or_default();
    let writable = |key: &str, s: &Map<String, Value>| {
        overwrite_admin || s.get(key).and_then(Value::as_str) != Some("admin")
    };
    // Claims `key` for `src` when the admin has not set it.
    let claim = |key: &str, src: MetadataSource, s: &mut Map<String, Value>| {
        let ok = writable(key, s);
        if ok {
            s.insert(key.to_string(), json!(src.as_str()));
        }
        ok
    };
    let text = |key: &'static str,
                get: fn(&CandidateData) -> Option<String>,
                s: &mut Map<String, Value>| {
        first(picks, |p| get(p.data)).and_then(|(v, src)| claim(key, src, s).then_some(v))
    };

    let mut set = Updates::default();
    let mut images: Vec<ImageTask> = Vec::new();
    for field in fields {
        match field {
            MetadataField::Title => set.title = text("title", |d| d.title.clone(), &mut sources),
            MetadataField::Summary => {
                set.summary = text("summary", |d| d.summary.clone(), &mut sources)
            }
            MetadataField::Description => {
                set.description = text("description", |d| d.description.clone(), &mut sources)
            }
            MetadataField::Developer => {
                set.developer = text("developer", |d| d.developer.clone(), &mut sources)
            }
            MetadataField::Publisher => {
                set.publisher = text("publisher", |d| d.publisher.clone(), &mut sources)
            }
            MetadataField::ReleaseDate => {
                let parsed = |p: &Pick<'_>| {
                    p.data
                        .release_date
                        .as_deref()
                        .and_then(|d| Date::parse(d, &Iso8601::DATE).ok())
                };
                set.release_date = first(picks, parsed)
                    .and_then(|(v, src)| claim("release_date", src, &mut sources).then_some(v));
            }
            MetadataField::Genres => {
                set.genres = first(picks, |p| {
                    Some(p.data.genres.clone()).filter(|g| !g.is_empty())
                })
                .and_then(|(v, src)| claim("genres", src, &mut sources).then_some(v));
            }
            MetadataField::ExternalIds => {
                let igdb = |p: &Pick<'_>| match p.source {
                    MetadataSource::Igdb => Some(p.external_id),
                    MetadataSource::Steam => p.data.external.igdb_id,
                };
                let steam = |p: &Pick<'_>| match p.source {
                    MetadataSource::Steam => Some(p.external_id),
                    MetadataSource::Igdb => p.data.external.steam_app_id,
                };
                set.igdb_id = first(picks, igdb)
                    .and_then(|(v, src)| claim("igdb_id", src, &mut sources).then_some(v));
                set.steam_app_id = first(picks, steam)
                    .and_then(|(v, src)| claim("steam_app_id", src, &mut sources).then_some(v));
            }
            MetadataField::Cover | MetadataField::Hero | MetadataField::Logo => {
                let (key, kind, url): (&str, AssetKind, ImageUrl) = match field {
                    MetadataField::Cover => ("cover", AssetKind::Cover, |d| d.images.cover.clone()),
                    MetadataField::Hero => ("hero", AssetKind::Hero, |d| d.images.hero.clone()),
                    _ => ("logo", AssetKind::Logo, |d| d.images.logo.clone()),
                };
                // The source is recorded once the image is actually stored.
                if let Some((url, source)) = first(picks, |p| url(p.data))
                    && writable(key, &sources)
                {
                    images.push(ImageTask { kind, source, url });
                }
            }
            MetadataField::Screenshots => {
                if let Some((urls, source)) = first(picks, |p| {
                    Some(p.data.images.screenshots.clone()).filter(|s| !s.is_empty())
                }) {
                    images.extend(urls.into_iter().map(|url| ImageTask {
                        kind: AssetKind::Screenshot,
                        source,
                        url,
                    }));
                }
            }
        }
    }

    sqlx::query!(
        r#"UPDATE packages SET
             title = COALESCE($2, title),
             summary = COALESCE($3, summary),
             description = COALESCE($4, description),
             developer = COALESCE($5, developer),
             publisher = COALESCE($6, publisher),
             release_date = COALESCE($7, release_date),
             genres = COALESCE($8, genres),
             steam_app_id = COALESCE($9, steam_app_id),
             igdb_id = COALESCE($10, igdb_id),
             field_sources = $11
           WHERE id = $1"#,
        package_id,
        set.title,
        set.summary,
        set.description,
        set.developer,
        set.publisher,
        set.release_date,
        set.genres.as_deref(),
        set.steam_app_id,
        set.igdb_id,
        Value::Object(sources)
    )
    .execute(&mut *conn)
    .await?;

    if !images.is_empty() {
        let payload = ImagesPayload {
            package_id,
            overwrite_admin,
            images,
        };
        jobs::enqueue(
            conn,
            "metadata.images",
            serde_json::to_value(&payload).map_err(ApiError::internal_from)?,
            Enqueue::default(),
        )
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// metadata.images
// ---------------------------------------------------------------------------------------

async fn images_job(ctx: JobContext) -> Result<(), JobError> {
    let p: ImagesPayload = serde_json::from_value(ctx.payload.clone())
        .map_err(|e| JobError::Fatal(format!("invalid payload: {e}")))?;
    let state = &ctx.state;
    let alive = sqlx::query_scalar!(
        r#"SELECT 1 AS "x!" FROM packages WHERE id = $1 AND deleted_at IS NULL"#,
        p.package_id
    )
    .fetch_optional(&state.db)
    .await?;
    if alive.is_none() {
        return Ok(());
    }
    let mut transient: Option<String> = None;
    for task in &p.images {
        let bytes = match state.metadata.images.get(&task.url).await {
            Ok(b) => b,
            Err(e) if e.is_transient() => {
                transient = Some(e.to_string());
                continue;
            }
            Err(e) => {
                tracing::warn!(kind = ?task.kind, error = %e, "metadata image skipped");
                continue;
            }
        };
        let img = match tokio::task::spawn_blocking(move || assets::process_image(&bytes)).await? {
            Ok(img) => img,
            Err(e) => {
                tracing::warn!(kind = ?task.kind, error = %e, "metadata image rejected");
                continue;
            }
        };
        let source = match task.source {
            MetadataSource::Igdb => AssetSource::Igdb,
            MetadataSource::Steam => AssetSource::Steam,
        };
        let asset =
            match assets::store_image(state, p.package_id, task.kind, img, source, Some(&task.url))
                .await
            {
                Ok((asset, _)) => asset,
                // A concurrent identical download won; nothing to do.
                Err(e) if e.status == StatusCode::CONFLICT => continue,
                Err(e) => return Err(JobError::Retry(e.to_string())),
            };
        let key = match task.kind {
            AssetKind::Cover => "cover",
            AssetKind::Hero => "hero",
            AssetKind::Logo => "logo",
            _ => continue,
        };
        sqlx::query!(
            r#"UPDATE packages SET
                 cover_asset_id = CASE WHEN $2 = 'cover' THEN $3 ELSE cover_asset_id END,
                 hero_asset_id = CASE WHEN $2 = 'hero' THEN $3 ELSE hero_asset_id END,
                 logo_asset_id = CASE WHEN $2 = 'logo' THEN $3 ELSE logo_asset_id END,
                 field_sources = field_sources || jsonb_build_object($2::text, $4::text)
               WHERE id = $1 AND ($5 OR field_sources->>$2 IS DISTINCT FROM 'admin')"#,
            p.package_id,
            key,
            asset.id,
            task.source.as_str(),
            p.overwrite_admin
        )
        .execute(&state.db)
        .await?;
    }
    match transient {
        Some(e) => Err(JobError::Retry(e)),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------------------

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

/// Re-query IGDB and Steam
#[utoipa::path(
    post,
    path = "/v1/admin/packages/{package_id}/metadata/refresh",
    tag = "admin-packages",
    operation_id = "adminRefreshMetadata",
    params(("package_id" = Uuid, Path)),
    responses((status = 202, description = "Job queued (or already queued)", body = Job), Unauthorized, Forbidden, NotFound)
)]
pub async fn refresh(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    ensure_package(&state, id).await?;
    let mut tx = state.db.begin().await?;
    if enqueue_fetch(&mut tx, id).await?.is_some() {
        audit::record(
            &mut tx,
            admin.user_id,
            &meta,
            "package.metadata_refresh",
            "package",
            &id.to_string(),
            json!({}),
        )
        .await?;
    }
    tx.commit().await?;
    let job = packages::metadata_jobs(&state, &[id])
        .await?
        .remove(&id)
        .ok_or_else(ApiError::internal)?;
    Ok((StatusCode::ACCEPTED, axum::Json(job)).into_response())
}

/// Candidates found by the last fetch
#[utoipa::path(
    get,
    path = "/v1/admin/packages/{package_id}/metadata/candidates",
    tag = "admin-packages",
    operation_id = "adminListMetadataCandidates",
    params(("package_id" = Uuid, Path)),
    responses(
        (status = 200, description = "Candidates, best score first", body = inline(MetadataCandidateList)),
        Unauthorized, Forbidden, NotFound
    )
)]
pub async fn list_candidates(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<axum::Json<MetadataCandidateList>> {
    ensure_package(&state, id).await?;
    let rows = sqlx::query!(
        r#"SELECT source, external_id, title, release_year, score, data, fetched_at
           FROM metadata_candidates WHERE package_id = $1
           ORDER BY score DESC, source, external_id"#,
        id
    )
    .fetch_all(&state.db)
    .await?;
    let items = rows
        .into_iter()
        .filter_map(|r| {
            Some(MetadataCandidate {
                source: MetadataSource::parse(&r.source)?,
                external_id: r.external_id,
                title: r.title,
                release_year: r.release_year.map(i32::from),
                score: r.score.clamp(0.0, 1.0),
                data: serde_json::from_value(r.data).ok()?,
                fetched_at: r.fetched_at,
            })
        })
        .collect();
    let job: Option<Job> = packages::metadata_jobs(&state, &[id]).await?.remove(&id);
    Ok(axum::Json(MetadataCandidateList { items, job }))
}

impl Validate for MetadataApply {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if self.external_id < 1 {
            invalid(errors, "external_id", "minimum", "must be at least 1");
        }
        if self.fields.is_empty() {
            invalid(errors, "fields", "min_items", "choose at least one field");
        }
    }
}

/// Apply chosen fields from a candidate
#[utoipa::path(
    post,
    path = "/v1/admin/packages/{package_id}/metadata/apply",
    tag = "admin-packages",
    operation_id = "adminApplyMetadata",
    params(("package_id" = Uuid, Path), ("If-Match" = String, Header)),
    request_body = MetadataApply,
    responses(
        (status = 200, description = "Updated package (images are fetched by a job)", body = AdminPackage,
         headers(("ETag" = String))),
        BadRequest, Unauthorized, Forbidden, NotFound, PreconditionFailed, PreconditionRequired
    )
)]
pub async fn apply(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Path(id): Path<Uuid>,
    if_match: IfMatch,
    Json(req): Json<MetadataApply>,
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
    let data = sqlx::query_scalar!(
        "SELECT data FROM metadata_candidates WHERE package_id = $1 AND source = $2 AND external_id = $3",
        id,
        req.source.as_str(),
        req.external_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::field("external_id", "unknown_candidate", "no such candidate for this package"))?;
    let data: CandidateData = serde_json::from_value(data).map_err(ApiError::internal_from)?;
    let pick = Pick {
        source: req.source,
        external_id: req.external_id,
        data: &data,
    };
    let fields: Vec<MetadataField> = req.fields.iter().copied().collect();
    apply_picks(&mut tx, id, &[pick], &fields, req.overwrite_admin_fields).await?;
    audit::record(
        &mut tx,
        admin.user_id,
        &meta,
        "package.metadata_apply",
        "package",
        &id.to_string(),
        json!({
            "source": req.source.as_str(),
            "external_id": req.external_id,
            "fields": req.fields,
            "overwrite_admin_fields": req.overwrite_admin_fields,
        }),
    )
    .await?;
    tx.commit().await?;
    let (pkg, updated) = packages::load_one_admin(&state, id).await?;
    Ok(packages::with_etag(
        axum::Json(pkg).into_response(),
        updated,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(source: MetadataSource, id: i64, steam: Option<i64>) -> Found {
        Found {
            source,
            external_id: id,
            title: "t".into(),
            release_year: None,
            data: CandidateData {
                external: vgames_proto::packages::CandidateExternal {
                    steam_app_id: steam,
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    }

    #[test]
    fn auto_apply_rule() {
        use MetadataSource::{Igdb, Steam};
        let one = vec![(1.0, found(Igdb, 1, None)), (0.6, found(Igdb, 2, None))];
        assert_eq!(auto_picks(&one, None, None), [0]);
        let two = vec![(1.0, found(Igdb, 1, None)), (0.97, found(Igdb, 2, None))];
        assert!(
            auto_picks(&two, None, None).is_empty(),
            "ambiguous titles never auto-apply"
        );
        let linked = vec![
            (1.0, found(Igdb, 1, Some(50))),
            (1.0, found(Steam, 50, Some(50))),
        ];
        assert_eq!(auto_picks(&linked, None, None), [0, 1]);
        let unlinked = vec![
            (1.0, found(Igdb, 1, Some(49))),
            (1.0, found(Steam, 50, Some(50))),
        ];
        assert!(auto_picks(&unlinked, None, None).is_empty());
        let weak = vec![(0.9, found(Steam, 50, Some(50)))];
        assert!(auto_picks(&weak, None, None).is_empty());
        // Explicit ids win regardless of score.
        let explicit = vec![
            (0.2, found(Igdb, 7, Some(50))),
            (0.1, found(Steam, 50, Some(50))),
        ];
        assert_eq!(auto_picks(&explicit, Some(7), Some(50)), [0, 1]);
        assert_eq!(auto_picks(&explicit, None, Some(50)), [0, 1]);
        assert!(auto_picks(&explicit, Some(8), None).is_empty());
    }
}
