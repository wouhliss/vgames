//! Cloud saves: content-addressed blobs, snapshots and the per-package head
//! (docs/architecture/06-cloud-saves.md).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Operating system a snapshot was taken on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// A blob the client wants stored: BLAKE3 of the file content and its size.
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

/// Always `PUT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum PutMethod {
    #[serde(rename = "PUT")]
    Put,
}

/// A signed single `PUT` of exactly the blob's size.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BlobUploadTarget {
    #[cfg_attr(feature = "openapi", schema(pattern = "^[0-9a-f]{64}$"))]
    pub blake3: String,
    #[cfg_attr(feature = "openapi", schema(format = "uri"))]
    pub url: String,
    #[cfg_attr(feature = "openapi", schema(inline))]
    pub method: PutMethod,
    /// Headers that must be sent exactly (they are part of the signature).
    pub headers: BTreeMap<String, String>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub expires_at: OffsetDateTime,
}

/// Blobs the server does not have yet, with upload targets. Blobs it already has are omitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PrepareBlobsResponse {
    pub missing: Vec<BlobUploadTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct BlobDownloadUrlsRequest {
    #[cfg_attr(feature = "openapi", schema(min_items = 1, max_items = 1000))]
    pub blake3: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BlobDownloadUrl {
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
pub struct BlobDownloadUrlList {
    pub items: Vec<BlobDownloadUrl>,
}

/// One file of a snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SaveFile {
    /// Save location id from the manifest (`saves.locations[].id`).
    #[cfg_attr(feature = "openapi", schema(pattern = "^[a-z0-9_-]{1,32}$"))]
    pub root: String,
    /// Relative, `/`-separated path under the location (package path rules).
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

/// A snapshot with its file list.
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CommitSnapshotRequest {
    /// The head this snapshot is based on; `null` only when no head exists yet.
    /// Required (send `null` explicitly).
    #[serde(deserialize_with = "Option::deserialize")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub parent_snapshot_id: Option<Uuid>,
    pub platform: OsFamily,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(max_length = 100))]
    pub label: Option<String>,
    #[cfg_attr(feature = "openapi", schema(max_items = 10000))]
    pub files: Vec<SaveFile>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn parent_snapshot_id_is_required_but_nullable() {
        let ok: CommitSnapshotRequest =
            serde_json::from_str(r#"{"parent_snapshot_id":null,"platform":"linux","files":[]}"#)
                .unwrap();
        assert_eq!(ok.parent_snapshot_id, None);
        let missing =
            serde_json::from_str::<CommitSnapshotRequest>(r#"{"platform":"linux","files":[]}"#);
        assert!(
            missing
                .unwrap_err()
                .to_string()
                .contains("parent_snapshot_id")
        );
    }

    #[test]
    fn snapshot_flattens_its_summary() {
        let s = SaveSnapshot {
            summary: SaveSnapshotSummary {
                id: Uuid::nil(),
                package_id: Uuid::nil(),
                parent_id: None,
                device_id: None,
                device_name: None,
                platform: OsFamily::Macos,
                file_count: 0,
                total_size: 0,
                label: None,
                created_at: OffsetDateTime::UNIX_EPOCH,
            },
            files: Vec::new(),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["platform"], "macos");
        assert_eq!(v["files"], serde_json::json!([]));
        assert!(v.get("summary").is_none());
    }
}
