//! Cloud saves (docs/architecture/06-cloud-saves.md).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum OsFamily {
    Windows,
    Linux,
    Macos,
}

impl OsFamily {
    pub fn as_str(self) -> &'static str {
        match self {
            OsFamily::Windows => "windows",
            OsFamily::Linux => "linux",
            OsFamily::Macos => "macos",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [OsFamily::Windows, OsFamily::Linux, OsFamily::Macos]
            .into_iter()
            .find(|o| o.as_str() == s)
    }
}

/// One file of a snapshot, stored as-is in `save_snapshots.files`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SaveFile {
    /// Save location id from the manifest.
    #[cfg_attr(feature = "openapi", schema(pattern = "^[a-z0-9_-]{1,32}$"))]
    pub root: String,
    #[cfg_attr(feature = "openapi", schema(max_length = 512))]
    pub path: String,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub size: i64,
    #[cfg_attr(feature = "openapi", schema(pattern = "^[0-9a-f]{64}$"))]
    pub blake3: String,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub mtime: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SaveSnapshotSummary {
    pub id: Uuid,
    pub package_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    pub platform: OsFamily,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub file_count: i32,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub total_size: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SaveSnapshot {
    #[serde(flatten)]
    pub summary: SaveSnapshotSummary,
    pub files: Vec<SaveFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SaveSnapshotPage {
    pub items: Vec<SaveSnapshotSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// A field that must be present but may be `null` (serde treats `Option` as optional otherwise).
fn present_or_null<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<Uuid>, D::Error> {
    Option::<Uuid>::deserialize(de)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CommitSnapshotRequest {
    /// The head this snapshot is based on; `null` only when no head exists yet (the key is required).
    #[serde(deserialize_with = "present_or_null")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub parent_snapshot_id: Option<Uuid>,
    pub platform: OsFamily,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(max_length = 100))]
    pub label: Option<String>,
    #[cfg_attr(feature = "openapi", schema(max_items = 10000))]
    pub files: Vec<SaveFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct BlobRef {
    #[cfg_attr(feature = "openapi", schema(pattern = "^[0-9a-f]{64}$"))]
    pub blake3: String,
    #[cfg_attr(feature = "openapi", schema(minimum = 0, maximum = 4294967296_i64))]
    pub size: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct PrepareBlobsRequest {
    #[cfg_attr(feature = "openapi", schema(max_items = 10000))]
    pub blobs: Vec<BlobRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BlobUploadTarget {
    #[cfg_attr(feature = "openapi", schema(pattern = "^[0-9a-f]{64}$"))]
    pub blake3: String,
    #[cfg_attr(feature = "openapi", schema(format = "uri"))]
    pub url: String,
    /// Always `PUT`.
    pub method: String,
    pub headers: BTreeMap<String, String>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub expires_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PrepareBlobsResponse {
    pub missing: Vec<BlobUploadTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct BlobDownloadRequest {
    #[cfg_attr(feature = "openapi", schema(min_items = 1, max_items = 1000))]
    pub blake3: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BlobUrl {
    #[cfg_attr(feature = "openapi", schema(pattern = "^[0-9a-f]{64}$"))]
    pub blake3: String,
    #[cfg_attr(feature = "openapi", schema(format = "uri"))]
    pub url: String,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub expires_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BlobUrlList {
    pub items: Vec<BlobUrl>,
}
