//! Admin server endpoints: allowlist, settings, jobs, audit (A1-T14).
//!
//! Every mutation writes `audit_log` in the transaction that performs it. Settings are
//! owner-only to change (`If-Match` required); admins may read them.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use utoipa::IntoParams;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_proto::{
    FieldError,
    admin::{
        AllowlistCreate, AllowlistEntry, AllowlistList, AuditEntry, AuditPage, ServerSettings,
        ServerSettingsPatch,
    },
    jobs::{Job, JobPage, JobState},
};

use crate::{
    audit,
    auth::{RequestMeta, RequireAdmin, RequireOwner},
    error::{ApiError, ApiResult, is_unique_violation},
    http::{
        etag::{IfMatch, etag, etag_header},
        json::{Json, JsonResponse, Validate, invalid},
        pagination::{CursorCodec, PageParams, finish_page},
        query::Query,
    },
    openapi_problems::{
        BadRequest, Conflict, Forbidden, NotFound, PreconditionFailed, PreconditionRequired,
        Unauthorized,
    },
    settings,
    state::AppState,
    users::user_public,
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_allowlist, add_allowlist))
        .routes(routes!(remove_allowlist))
        .routes(routes!(get_settings, update_settings))
        .routes(routes!(list_jobs))
        .routes(routes!(retry_job))
        .routes(routes!(list_audit))
}

fn valid_discord_id(s: &str) -> bool {
    (5..=25).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

fn page_errors(limit: Option<u32>, cursor: &Option<String>, errors: &mut Vec<FieldError>) {
    PageParams {
        limit,
        cursor: cursor.clone(),
    }
    .validate(errors);
}

fn max_chars(errors: &mut Vec<FieldError>, field: &str, value: Option<&str>, max: usize) {
    if value.is_some_and(|v| v.chars().count() > max) {
        invalid(
            errors,
            field,
            "max_length",
            format!("must be at most {max} characters"),
        );
    }
}

// ---------------------------------------------------------------------------------------
// Allowlist
// ---------------------------------------------------------------------------------------

struct AllowlistRow {
    discord_id: String,
    note: Option<String>,
    created_at: OffsetDateTime,
    added_by_id: Option<Uuid>,
    added_by_username: Option<String>,
    added_by_display_name: Option<String>,
    added_by_discord_id: Option<String>,
    added_by_avatar_hash: Option<String>,
}

impl AllowlistRow {
    fn into_entry(self) -> AllowlistEntry {
        let added_by = match (
            self.added_by_id,
            self.added_by_username,
            self.added_by_discord_id,
        ) {
            (Some(id), Some(username), Some(discord_id)) => Some(user_public(
                id,
                username,
                self.added_by_display_name,
                &discord_id,
                self.added_by_avatar_hash.as_deref(),
            )),
            _ => None,
        };
        AllowlistEntry {
            discord_id: self.discord_id,
            note: self.note,
            added_by,
            created_at: self.created_at,
        }
    }
}

/// Registration allowlist
#[utoipa::path(
    get,
    path = "/v1/admin/allowlist",
    tag = "admin-server",
    operation_id = "adminListAllowlist",
    responses((status = 200, description = "Entries", body = inline(AllowlistList)), Unauthorized, Forbidden)
)]
pub async fn list_allowlist(
    State(state): State<AppState>,
    _admin: RequireAdmin,
) -> ApiResult<axum::Json<AllowlistList>> {
    let rows = sqlx::query_as!(
        AllowlistRow,
        r#"SELECT a.discord_id, a.note, a.created_at,
                  u.id AS "added_by_id?", u.username AS "added_by_username?",
                  u.display_name AS "added_by_display_name?", u.discord_id AS "added_by_discord_id?",
                  u.avatar_hash AS "added_by_avatar_hash?"
           FROM registration_allowlist a LEFT JOIN users u ON u.id = a.added_by
           ORDER BY a.created_at DESC, a.discord_id"#
    )
    .fetch_all(&state.db)
    .await?;
    Ok(axum::Json(AllowlistList {
        items: rows.into_iter().map(AllowlistRow::into_entry).collect(),
    }))
}

impl Validate for AllowlistCreate {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if !valid_discord_id(&self.discord_id) {
            invalid(errors, "discord_id", "invalid", "must be 5-25 digits");
        }
        max_chars(errors, "note", self.note.as_deref(), 200);
    }
}

/// Allow a Discord account to sign in
#[utoipa::path(
    post,
    path = "/v1/admin/allowlist",
    tag = "admin-server",
    operation_id = "adminAddAllowlist",
    request_body = AllowlistCreate,
    responses(
        (status = 201, description = "Added", body = AllowlistEntry),
        BadRequest, Unauthorized, Forbidden, Conflict
    )
)]
pub async fn add_allowlist(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Json(req): Json<AllowlistCreate>,
) -> ApiResult<JsonResponse<AllowlistEntry>> {
    let note = req.note.as_deref().map(str::trim).filter(|n| !n.is_empty());
    let mut tx = state.db.begin().await?;
    sqlx::query!(
        "INSERT INTO registration_allowlist (discord_id, note, added_by) VALUES ($1, $2, $3)",
        req.discord_id,
        note,
        admin.user_id
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e, None) {
            ApiError::conflict(
                "already_allowlisted",
                "This Discord account is already on the allowlist",
            )
        } else {
            e.into()
        }
    })?;
    audit::record(
        &mut tx,
        admin.user_id,
        &meta,
        "allowlist.add",
        "discord_account",
        &req.discord_id,
        json!({ "note": note }),
    )
    .await?;
    tx.commit().await?;
    let row = sqlx::query_as!(
        AllowlistRow,
        r#"SELECT a.discord_id, a.note, a.created_at,
                  u.id AS "added_by_id?", u.username AS "added_by_username?",
                  u.display_name AS "added_by_display_name?", u.discord_id AS "added_by_discord_id?",
                  u.avatar_hash AS "added_by_avatar_hash?"
           FROM registration_allowlist a LEFT JOIN users u ON u.id = a.added_by
           WHERE a.discord_id = $1"#,
        req.discord_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok(JsonResponse(StatusCode::CREATED, row.into_entry()))
}

/// Remove from allowlist (existing sessions are unaffected)
#[utoipa::path(
    delete,
    path = "/v1/admin/allowlist/{discord_id}",
    tag = "admin-server",
    operation_id = "adminRemoveAllowlist",
    params(("discord_id" = String, Path, pattern = "^[0-9]{5,25}$")),
    responses((status = 204, description = "Removed"), Unauthorized, Forbidden, NotFound)
)]
pub async fn remove_allowlist(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Path(discord_id): Path<String>,
) -> ApiResult<StatusCode> {
    if !valid_discord_id(&discord_id) {
        return Err(ApiError::not_found());
    }
    let mut tx = state.db.begin().await?;
    let note = sqlx::query_scalar!(
        "DELETE FROM registration_allowlist WHERE discord_id = $1 RETURNING note",
        discord_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    audit::record(
        &mut tx,
        admin.user_id,
        &meta,
        "allowlist.remove",
        "discord_account",
        &discord_id,
        json!({ "note": note }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------

fn settings_body(state: &AppState, s: &settings::Settings) -> ServerSettings {
    ServerSettings {
        registration_mode: s.registration_mode,
        name: s.name_or(&state.config.server_name).to_string(),
        motd: s.motd.clone(),
    }
}

fn settings_response(state: &AppState, s: &settings::Settings) -> Response {
    let mut resp = axum::Json(settings_body(state, s)).into_response();
    let (k, v) = etag_header(s.updated_at);
    resp.headers_mut().insert(k, v);
    resp
}

/// Server settings
#[utoipa::path(
    get,
    path = "/v1/admin/settings",
    tag = "admin-server",
    operation_id = "adminGetSettings",
    responses(
        (status = 200, description = "Settings", body = ServerSettings, headers(("ETag" = String))),
        Unauthorized, Forbidden
    )
)]
pub async fn get_settings(
    State(state): State<AppState>,
    _admin: RequireAdmin,
) -> ApiResult<Response> {
    let s = settings::load(&state.db).await?;
    Ok(settings_response(&state, &s))
}

impl Validate for ServerSettingsPatch {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if let Some(name) = &self.name {
            let n = name.trim().chars().count();
            if !(1..=100).contains(&n) {
                invalid(errors, "name", "length", "must be 1-100 characters");
            }
        }
        max_chars(errors, "motd", self.motd.as_deref(), 500);
    }
}

/// Update server settings (owner only)
#[utoipa::path(
    patch,
    path = "/v1/admin/settings",
    tag = "admin-server",
    operation_id = "adminUpdateSettings",
    params(("If-Match" = String, Header)),
    request_body(content = ServerSettingsPatch, content_type = "application/merge-patch+json"),
    responses(
        (status = 200, description = "Updated", body = ServerSettings, headers(("ETag" = String))),
        BadRequest, Unauthorized, Forbidden, PreconditionFailed, PreconditionRequired
    )
)]
pub async fn update_settings(
    State(state): State<AppState>,
    RequireOwner(owner): RequireOwner,
    meta: RequestMeta,
    if_match: IfMatch,
    Json(patch): Json<ServerSettingsPatch>,
) -> ApiResult<Response> {
    let mut tx = state.db.begin().await?;
    // Lock the rows so the If-Match check and the write see the same version.
    let current = sqlx::query_scalar!(
        r#"SELECT max(updated_at) AS "updated_at!" FROM (
             SELECT updated_at FROM server_settings WHERE key = ANY($1) FOR UPDATE) s"#,
        &settings::KEYS.map(str::to_string)[..]
    )
    .fetch_one(&mut *tx)
    .await?;
    if_match.check(&etag(current))?;

    let mut changes: Vec<(&str, Value)> = Vec::new();
    if let Some(mode) = patch.registration_mode {
        changes.push((
            settings::REGISTRATION_MODE,
            serde_json::to_value(mode).map_err(ApiError::internal_from)?,
        ));
    }
    if let Some(name) = &patch.name {
        changes.push((settings::NAME, json!(name.trim())));
    }
    if let Some(motd) = &patch.motd {
        changes.push((settings::MOTD, json!(motd.trim())));
    }
    let mut details = Map::new();
    for (key, value) in &changes {
        sqlx::query!(
            r#"INSERT INTO server_settings (key, value, updated_at, updated_by) VALUES ($1, $2, now(), $3)
               ON CONFLICT (key) DO UPDATE
                 SET value = EXCLUDED.value, updated_at = now(), updated_by = EXCLUDED.updated_by"#,
            key,
            value,
            owner.user_id
        )
        .execute(&mut *tx)
        .await?;
        details.insert((*key).to_string(), value.clone());
    }
    if !changes.is_empty() {
        audit::record(
            &mut tx,
            owner.user_id,
            &meta,
            "settings.update",
            "server_settings",
            "server",
            Value::Object(details),
        )
        .await?;
    }
    tx.commit().await?;
    let s = settings::load(&state.db).await?;
    Ok(settings_response(&state, &s))
}

// ---------------------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------------------

struct JobRow {
    id: Uuid,
    kind: String,
    state: String,
    attempts: i32,
    max_attempts: i32,
    last_error: Option<String>,
    run_at: OffsetDateTime,
    created_at: OffsetDateTime,
    finished_at: Option<OffsetDateTime>,
}

impl JobRow {
    fn into_job(self) -> Job {
        Job {
            id: self.id,
            kind: self.kind,
            state: JobState::parse(&self.state).unwrap_or(JobState::Queued),
            attempts: self.attempts,
            max_attempts: self.max_attempts,
            last_error: self.last_error,
            run_at: Some(self.run_at),
            created_at: self.created_at,
            finished_at: self.finished_at,
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct JobListQuery {
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    #[param(inline)]
    pub state: Option<JobState>,
    #[param(max_length = 64)]
    pub kind: Option<String>,
}

impl Validate for JobListQuery {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        page_errors(self.limit, &self.cursor, errors);
        max_chars(errors, "kind", self.kind.as_deref(), 64);
    }
}

/// Background jobs
#[utoipa::path(
    get,
    path = "/v1/admin/jobs",
    tag = "admin-server",
    operation_id = "adminListJobs",
    params(JobListQuery),
    responses((status = 200, description = "Page of jobs", body = JobPage), Unauthorized, Forbidden)
)]
pub async fn list_jobs(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Query(q): Query<JobListQuery>,
) -> ApiResult<axum::Json<JobPage>> {
    let limit = q.limit.unwrap_or(crate::http::pagination::DEFAULT_LIMIT);
    let job_state = q.state.map(JobState::as_str);
    let filters = json!({ "state": job_state, "kind": q.kind });
    let codec = CursorCodec::new(state.keys.cursor.expose());
    let after: Option<Uuid> = q
        .cursor
        .as_deref()
        .map(|c| codec.decode(c, &filters))
        .transpose()?
        .map(|(_, id): (String, Uuid)| id);
    let rows = sqlx::query_as!(
        JobRow,
        r#"SELECT id, kind, state, attempts, max_attempts, last_error, run_at, created_at, finished_at
           FROM jobs
           WHERE ($1::uuid IS NULL OR id < $1)
             AND ($2::text IS NULL OR state = $2)
             AND ($3::text IS NULL OR kind = $3)
           ORDER BY id DESC LIMIT $4"#,
        after,
        job_state,
        q.kind,
        i64::from(limit) + 1
    )
    .fetch_all(&state.db)
    .await?;
    let page = finish_page(rows, limit, |r| {
        codec.encode(&String::new(), r.id, &filters)
    })?;
    Ok(axum::Json(JobPage {
        items: page.items.into_iter().map(JobRow::into_job).collect(),
        next_cursor: page.next_cursor,
    }))
}

/// Requeue a failed or dead job
#[utoipa::path(
    post,
    path = "/v1/admin/jobs/{job_id}/retry",
    tag = "admin-server",
    operation_id = "adminRetryJob",
    params(("job_id" = Uuid, Path)),
    responses(
        (status = 200, description = "Requeued", body = Job),
        Unauthorized, Forbidden, NotFound, Conflict
    )
)]
pub async fn retry_job(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    meta: RequestMeta,
    Path(job_id): Path<Uuid>,
) -> ApiResult<axum::Json<Job>> {
    let mut tx = state.db.begin().await?;
    let previous = sqlx::query_scalar!("SELECT state FROM jobs WHERE id = $1 FOR UPDATE", job_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let row = sqlx::query_as!(
        JobRow,
        r#"UPDATE jobs
           SET state = 'queued', run_at = now(), attempts = 0, finished_at = NULL,
               locked_by = NULL, locked_until = NULL
           WHERE id = $1 AND state = ANY($2)
           RETURNING id, kind, state, attempts, max_attempts, last_error, run_at, created_at, finished_at"#,
        job_id,
        &[JobState::Failed.as_str().to_string(), JobState::Dead.as_str().to_string()]
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e, None) {
            ApiError::conflict(
                "job_already_queued",
                "An identical job is already queued or running",
            )
        } else {
            e.into()
        }
    })?
    .ok_or_else(|| {
        ApiError::conflict(
            "job_not_retryable",
            "Only failed or dead jobs can be retried",
        )
    })?;
    audit::record(
        &mut tx,
        admin.user_id,
        &meta,
        "job.retry",
        "job",
        &job_id.to_string(),
        json!({ "kind": row.kind, "previous_state": previous }),
    )
    .await?;
    tx.commit().await?;
    Ok(axum::Json(row.into_job()))
}

// ---------------------------------------------------------------------------------------
// Audit log
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct AuditQuery {
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    pub actor_user_id: Option<Uuid>,
    #[param(max_length = 64)]
    pub action: Option<String>,
    #[param(max_length = 64)]
    pub target_type: Option<String>,
    #[param(max_length = 128)]
    pub target_id: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[param(value_type = Option<String>, format = DateTime)]
    pub since: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[param(value_type = Option<String>, format = DateTime)]
    pub until: Option<OffsetDateTime>,
}

impl Validate for AuditQuery {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        page_errors(self.limit, &self.cursor, errors);
        max_chars(errors, "action", self.action.as_deref(), 64);
        max_chars(errors, "target_type", self.target_type.as_deref(), 64);
        max_chars(errors, "target_id", self.target_id.as_deref(), 128);
    }
}

/// Audit trail (newest first)
#[utoipa::path(
    get,
    path = "/v1/admin/audit-log",
    tag = "admin-server",
    operation_id = "adminListAuditLog",
    params(AuditQuery),
    responses((status = 200, description = "Page of entries", body = AuditPage), BadRequest, Unauthorized, Forbidden)
)]
pub async fn list_audit(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Query(q): Query<AuditQuery>,
) -> ApiResult<axum::Json<AuditPage>> {
    let limit = q.limit.unwrap_or(crate::http::pagination::DEFAULT_LIMIT);
    let filters = json!({
        "actor": q.actor_user_id, "action": q.action, "target_type": q.target_type,
        "target_id": q.target_id, "since": q.since.map(|t| t.unix_timestamp_nanos().to_string()),
        "until": q.until.map(|t| t.unix_timestamp_nanos().to_string()),
    });
    let codec = CursorCodec::new(state.keys.cursor.expose());
    let after: Option<Uuid> = q
        .cursor
        .as_deref()
        .map(|c| codec.decode(c, &filters))
        .transpose()?
        .map(|(_, id): (String, Uuid)| id);
    // UUIDv7 ids sort by creation time: newest first.
    let rows = sqlx::query!(
        r#"SELECT a.id, a.action, a.target_type, a.target_id, host(a.ip) AS ip, a.details, a.created_at,
                  u.id AS "actor_id?", u.username AS "actor_username?", u.display_name AS "actor_display_name?",
                  u.discord_id AS "actor_discord_id?", u.avatar_hash AS "actor_avatar_hash?"
           FROM audit_log a LEFT JOIN users u ON u.id = a.actor_user_id
           WHERE ($1::uuid IS NULL OR a.id < $1)
             AND ($2::uuid IS NULL OR a.actor_user_id = $2)
             AND ($3::text IS NULL OR a.action = $3)
             AND ($4::text IS NULL OR a.target_type = $4)
             AND ($5::text IS NULL OR a.target_id = $5)
             AND ($6::timestamptz IS NULL OR a.created_at >= $6)
             AND ($7::timestamptz IS NULL OR a.created_at <= $7)
           ORDER BY a.id DESC LIMIT $8"#,
        after,
        q.actor_user_id,
        q.action,
        q.target_type,
        q.target_id,
        q.since,
        q.until,
        i64::from(limit) + 1
    )
    .fetch_all(&state.db)
    .await?;
    let page = finish_page(rows, limit, |r| {
        codec.encode(&String::new(), r.id, &filters)
    })?;
    let items = page
        .items
        .into_iter()
        .map(|r| {
            let actor = match (r.actor_id, r.actor_username, r.actor_discord_id) {
                (Some(id), Some(username), Some(discord_id)) => Some(user_public(
                    id,
                    username,
                    r.actor_display_name,
                    &discord_id,
                    r.actor_avatar_hash.as_deref(),
                )),
                _ => None,
            };
            AuditEntry {
                id: r.id,
                actor,
                action: r.action,
                target_type: r.target_type,
                target_id: r.target_id,
                ip: r.ip,
                details: Some(r.details),
                created_at: r.created_at,
            }
        })
        .collect();
    Ok(axum::Json(AuditPage {
        items,
        next_cursor: page.next_cursor,
    }))
}
