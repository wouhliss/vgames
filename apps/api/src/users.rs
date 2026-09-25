//! Users and roles for admins (A1-T14, 01-security §4.3).
//!
//! Rules: only an owner changes roles; the last active owner cannot be demoted or disabled;
//! admins cannot disable or re-enable admins or owners; nobody can disable themselves.
//! Disabling revokes every session in the same transaction and closes the user's sockets
//! (`session.revoked`) once it commits.

use axum::extract::State;
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;
use utoipa::IntoParams;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_proto::{
    FieldError,
    admin::{AdminUser, AdminUserPage, AdminUserPatch},
    auth::{Role, User, UserPublic},
};

use crate::http::path::Path;
use crate::{
    audit,
    auth::{
        RequestMeta, RequireAdmin,
        discord::avatar_url,
        extract::{parse_role, role_str},
    },
    error::{ApiError, ApiResult},
    http::{
        json::{Json, Validate, invalid},
        pagination::{CursorCodec, PageParams, finish_page},
        query::Query,
    },
    openapi_problems::{BadRequest, Conflict, Forbidden, NotFound, Unauthorized},
    realtime::{bus, hub::Target},
    state::AppState,
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_users))
        .routes(routes!(update_user))
}

/// Public profile fields of a user row.
pub fn user_public(
    id: Uuid,
    username: String,
    display_name: Option<String>,
    discord_id: &str,
    avatar_hash: Option<&str>,
) -> UserPublic {
    UserPublic {
        id,
        username,
        display_name,
        avatar_url: avatar_url(discord_id, avatar_hash),
    }
}

struct UserRow {
    id: Uuid,
    discord_id: String,
    username: String,
    display_name: Option<String>,
    avatar_hash: Option<String>,
    role: String,
    created_at: OffsetDateTime,
    disabled_at: Option<OffsetDateTime>,
    disabled_reason: Option<String>,
    last_seen_at: Option<OffsetDateTime>,
}

impl UserRow {
    fn into_admin(self) -> AdminUser {
        AdminUser {
            user: User {
                avatar_url: avatar_url(&self.discord_id, self.avatar_hash.as_deref()),
                id: self.id,
                discord_id: Some(self.discord_id),
                username: self.username,
                display_name: self.display_name,
                role: parse_role(&self.role),
                created_at: self.created_at,
            },
            disabled_at: self.disabled_at,
            disabled_reason: self.disabled_reason,
            last_seen_at: self.last_seen_at,
        }
    }
}

async fn load_admin_user(state: &AppState, id: Uuid) -> ApiResult<AdminUser> {
    sqlx::query_as!(
        UserRow,
        r#"SELECT id, discord_id, username, display_name, avatar_hash, role, created_at,
                  disabled_at, disabled_reason, last_seen_at
           FROM users WHERE id = $1"#,
        id
    )
    .fetch_optional(&state.db)
    .await?
    .map(UserRow::into_admin)
    .ok_or_else(ApiError::not_found)
}

/// Escapes `%`, `_` and `\` for a `LIKE … ESCAPE '\'` pattern.
pub fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct UserListQuery {
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    /// Username or Discord id
    #[param(max_length = 64)]
    pub q: Option<String>,
    #[param(inline)]
    pub role: Option<Role>,
}

impl Validate for UserListQuery {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        PageParams {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
        .validate(errors);
        if self.q.as_ref().is_some_and(|q| q.chars().count() > 64) {
            invalid(errors, "q", "max_length", "must be at most 64 characters");
        }
    }
}

/// Users
#[utoipa::path(
    get,
    path = "/v1/admin/users",
    tag = "admin-server",
    operation_id = "adminListUsers",
    params(UserListQuery),
    responses((status = 200, description = "Page of users", body = AdminUserPage), Unauthorized, Forbidden)
)]
pub async fn list_users(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Query(q): Query<UserListQuery>,
) -> ApiResult<axum::Json<AdminUserPage>> {
    let limit = q.limit.unwrap_or(crate::http::pagination::DEFAULT_LIMIT);
    let search =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    let role = q.role.map(role_str);
    let filters = json!({ "q": search, "role": role });
    let codec = CursorCodec::new(state.keys.cursor.expose());
    let after: Option<Uuid> = q
        .cursor
        .as_deref()
        .map(|c| codec.decode(c, &filters))
        .transpose()?
        .map(|(_, id): (String, Uuid)| id);
    let pattern = search.as_deref().map(|s| like_escape(&s.to_lowercase()));
    // UUIDv7 ids sort by creation time: newest first.
    let rows = sqlx::query_as!(
        UserRow,
        r#"SELECT id, discord_id, username, display_name, avatar_hash, role, created_at,
                  disabled_at, disabled_reason, last_seen_at
           FROM users
           WHERE ($1::uuid IS NULL OR id < $1)
             AND ($2::text IS NULL OR role = $2)
             AND ($3::text IS NULL
                  OR lower(username) LIKE '%' || $3 || '%' ESCAPE '\'
                  OR lower(coalesce(display_name, '')) LIKE '%' || $3 || '%' ESCAPE '\'
                  OR discord_id = $4)
           ORDER BY id DESC LIMIT $5"#,
        after,
        role,
        pattern,
        search,
        i64::from(limit) + 1
    )
    .fetch_all(&state.db)
    .await?;
    let page = finish_page(rows, limit, |r| {
        codec.encode(&String::new(), r.id, &filters)
    })?;
    Ok(axum::Json(AdminUserPage {
        items: page.items.into_iter().map(UserRow::into_admin).collect(),
        next_cursor: page.next_cursor,
    }))
}

impl Validate for AdminUserPatch {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if let Some(reason) = &self.disabled_reason {
            if reason.chars().count() > 500 {
                invalid(
                    errors,
                    "disabled_reason",
                    "max_length",
                    "must be at most 500 characters",
                );
            }
            if self.disabled != Some(true) {
                invalid(
                    errors,
                    "disabled_reason",
                    "requires_disabled",
                    "only allowed together with \"disabled\": true",
                );
            }
        }
    }
}

fn last_owner() -> ApiError {
    ApiError::conflict(
        "last_owner",
        "The server must keep at least one active owner",
    )
}

/// Disable/enable a user; change role (owner only)
#[utoipa::path(
    patch,
    path = "/v1/admin/users/{user_id}",
    tag = "admin-server",
    operation_id = "adminUpdateUser",
    params(("user_id" = Uuid, Path)),
    request_body(content = AdminUserPatch, content_type = "application/merge-patch+json"),
    responses(
        (status = 200, description = "Updated", body = AdminUser),
        BadRequest, Unauthorized, Forbidden, NotFound, Conflict
    )
)]
pub async fn update_user(
    State(state): State<AppState>,
    RequireAdmin(caller): RequireAdmin,
    meta: RequestMeta,
    Path(user_id): Path<Uuid>,
    Json(patch): Json<AdminUserPatch>,
) -> ApiResult<axum::Json<AdminUser>> {
    if patch.role.is_some() && caller.role != Role::Owner {
        return Err(ApiError::forbidden().with_detail("Only an owner can change roles."));
    }
    let mut tx = state.db.begin().await?;
    // Lock the active owners first (always in the same order), so two concurrent changes
    // can never both remove "the other" owner.
    let owners = sqlx::query_scalar!(
        "SELECT id FROM users WHERE role = 'owner' AND disabled_at IS NULL ORDER BY id FOR UPDATE"
    )
    .fetch_all(&mut *tx)
    .await?;
    let target = sqlx::query!(
        "SELECT role, disabled_at FROM users WHERE id = $1 FOR UPDATE",
        user_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let target_role = parse_role(&target.role);
    let target_active = target.disabled_at.is_none();
    let is_active_owner = target_role == Role::Owner && target_active;

    if let Some(disabled) = patch.disabled {
        if disabled && user_id == caller.user_id {
            return Err(ApiError::forbidden_code(
                "cannot_disable_self",
                "You cannot disable your own account",
            ));
        }
        if caller.role == Role::Admin && target_role >= Role::Admin {
            return Err(ApiError::forbidden()
                .with_detail("Admins cannot disable or enable other admins or owners."));
        }
        if disabled && is_active_owner && owners.len() <= 1 {
            return Err(last_owner());
        }
    }
    if let Some(role) = patch.role
        && is_active_owner
        && role != Role::Owner
        && owners.len() <= 1
    {
        return Err(last_owner());
    }

    if let Some(role) = patch.role.filter(|r| *r != target_role) {
        sqlx::query!(
            "UPDATE users SET role = $2 WHERE id = $1",
            user_id,
            role_str(role)
        )
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut tx,
            caller.user_id,
            &meta,
            "user.role_change",
            "user",
            &user_id.to_string(),
            json!({ "from": role_str(target_role), "to": role_str(role) }),
        )
        .await?;
    }
    match patch.disabled {
        Some(true) => {
            let reason = patch
                .disabled_reason
                .as_deref()
                .map(str::trim)
                .filter(|r| !r.is_empty());
            sqlx::query!(
                r#"UPDATE users SET disabled_at = COALESCE(disabled_at, now()),
                          disabled_reason = COALESCE($2, disabled_reason)
                   WHERE id = $1"#,
                user_id,
                reason
            )
            .execute(&mut *tx)
            .await?;
            // The row is locked: `target_active` is the state we just changed.
            let newly = target_active;
            let revoked = sqlx::query!(
                "UPDATE sessions SET revoked_at = now(), revoked_reason = 'user_disabled' WHERE user_id = $1 AND revoked_at IS NULL",
                user_id
            )
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if newly || revoked > 0 {
                audit::record(
                    &mut tx,
                    caller.user_id,
                    &meta,
                    "user.disable",
                    "user",
                    &user_id.to_string(),
                    json!({ "reason": reason, "sessions_revoked": revoked }),
                )
                .await?;
                // Delivered only if this transaction commits.
                bus::publish_tx(
                    &mut tx,
                    &Target {
                        users: vec![user_id],
                        sessions: None,
                        close: true,
                    },
                    "session.revoked",
                    vgames_proto::realtime::SessionRevoked {
                        reason: "user_disabled".into(),
                    },
                )
                .await?;
            }
        }
        Some(false) if !target_active => {
            sqlx::query!(
                "UPDATE users SET disabled_at = NULL, disabled_reason = NULL WHERE id = $1",
                user_id
            )
            .execute(&mut *tx)
            .await?;
            audit::record(
                &mut tx,
                caller.user_id,
                &meta,
                "user.enable",
                "user",
                &user_id.to_string(),
                json!({}),
            )
            .await?;
        }
        _ => {}
    }
    tx.commit().await?;
    Ok(axum::Json(load_admin_user(&state, user_id).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_patterns_are_escaped() {
        assert_eq!(like_escape("a_b%c\\d"), "a\\_b\\%c\\\\d");
        assert_eq!(like_escape("plain"), "plain");
    }
}
