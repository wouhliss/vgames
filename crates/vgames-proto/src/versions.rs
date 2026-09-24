//! Package versions and upload targets (docs/architecture/02-package-format.md §6).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{auth::UserPublic, packages::Platform};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum VersionState {
    Uploading,
    Verifying,
    Ready,
    Published,
    Failed,
    Yanked,
    Aborted,
}

impl VersionState {
    pub fn as_str(self) -> &'static str {
        match self {
            VersionState::Uploading => "uploading",
            VersionState::Verifying => "verifying",
            VersionState::Ready => "ready",
            VersionState::Published => "published",
            VersionState::Failed => "failed",
            VersionState::Yanked => "yanked",
            VersionState::Aborted => "aborted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            VersionState::Uploading,
            VersionState::Verifying,
            VersionState::Ready,
            VersionState::Published,
            VersionState::Failed,
            VersionState::Yanked,
            VersionState::Aborted,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct VersionCreate {
    pub platform: Platform,
    #[cfg_attr(feature = "openapi", schema(min_length = 1, max_length = 64))]
    pub version_label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Version {
    pub id: Uuid,
    pub package_id: Uuid,
    pub server_id: Uuid,
    pub platform: Platform,
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub sequence: i64,
    pub version_label: String,
    pub state: VersionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_current_release: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub total_size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub file_count: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub chunk_count: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub pack_count: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(minimum = 0, maximum = 1))]
    pub verify_progress: Option<f32>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
    pub created_by: UserPublic,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = DateTime))]
    pub finalized_at: Option<OffsetDateTime>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = DateTime))]
    pub verified_at: Option<OffsetDateTime>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = DateTime))]
    pub published_at: Option<OffsetDateTime>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = DateTime))]
    pub yanked_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VersionPage {
    pub items: Vec<Version>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum UploadMethod {
    #[serde(rename = "POST")]
    Post,
    #[serde(rename = "PUT")]
    Put,
}

/// A signed request the client sends to object storage as-is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UploadTarget {
    #[cfg_attr(feature = "openapi", schema(format = "uri"))]
    pub url: String,
    #[cfg_attr(feature = "openapi", schema(inline))]
    pub method: UploadMethod,
    /// Headers that must be sent exactly (they are part of the signature).
    pub headers: BTreeMap<String, String>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub expires_at: OffsetDateTime,
}
