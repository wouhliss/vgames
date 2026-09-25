//! Server administration: users and roles, the registration allowlist, server settings
//! and the audit trail (docs/architecture/03-api.md §4, admin-server tag).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    auth::{Role, User, UserPublic},
    discovery::RegistrationMode,
};

/// A user as admins see them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AdminUser {
    #[serde(flatten)]
    pub user: User,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = DateTime))]
    pub disabled_at: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = DateTime))]
    pub last_seen_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AdminUserPage {
    pub items: Vec<AdminUser>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// `PATCH /v1/admin/users/{id}` (JSON Merge Patch). Role changes are owner-only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct AdminUserPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    /// Shown to other admins; only with `disabled: true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(max_length = 500))]
    pub disabled_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AllowlistEntry {
    #[cfg_attr(feature = "openapi", schema(pattern = "^[0-9]{5,25}$"))]
    pub discord_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<UserPublic>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AllowlistList {
    pub items: Vec<AllowlistEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct AllowlistCreate {
    #[cfg_attr(feature = "openapi", schema(pattern = "^[0-9]{5,25}$"))]
    pub discord_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(max_length = 200))]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ServerSettings {
    #[cfg_attr(feature = "openapi", schema(inline))]
    pub registration_mode: RegistrationMode,
    #[cfg_attr(feature = "openapi", schema(max_length = 100))]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(max_length = 500))]
    pub motd: Option<String>,
}

/// `PATCH /v1/admin/settings` (owner, If-Match). An empty `motd` clears it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ServerSettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(inline))]
    pub registration_mode: Option<RegistrationMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(min_length = 1, max_length = 100))]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(max_length = 500))]
    pub motd: Option<String>,
}

/// One audit-trail row (append-only).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AuditEntry {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<UserPublic>,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    /// Action-specific fields (always sent by this server; `{}` when there are none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub details: Option<serde_json::Value>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AuditPage {
    pub items: Vec<AuditEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
