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

/// A `vgames.sig/1` signature envelope (docs/architecture/01-security.md §3.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SignatureEnvelope {
    /// Always `vgames.sig/1`.
    pub format: String,
    /// Always `ed25519`.
    pub alg: String,
    /// `vgames/manifest/v1` or `vgames/compat/v1`.
    #[cfg_attr(feature = "openapi", schema(inline))]
    pub context: SignatureContext,
    #[cfg_attr(feature = "openapi", schema(pattern = "^[0-9a-f]{32}$"))]
    pub key_id: String,
    #[cfg_attr(feature = "openapi", schema(pattern = "^[0-9a-f]{64}$"))]
    pub payload_blake3: String,
    /// Standard base64 of the 64-byte Ed25519 signature.
    pub signature: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum SignatureContext {
    #[serde(rename = "vgames/manifest/v1")]
    Manifest,
    #[serde(rename = "vgames/compat/v1")]
    Compat,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct FinalizeRequest {
    #[cfg_attr(feature = "openapi", schema(minimum = 1, maximum = 268435456))]
    pub manifest_size: i64,
    #[cfg_attr(feature = "openapi", schema(pattern = "^[0-9a-f]{64}$"))]
    pub manifest_blake3: String,
    pub signature: SignatureEnvelope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ReplaceSignature {
    pub signature: SignatureEnvelope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct YankRequest {
    #[cfg_attr(feature = "openapi", schema(min_length = 3, max_length = 500))]
    pub reason: String,
}

/// Signed link to a version's `manifest.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ManifestLink {
    #[cfg_attr(feature = "openapi", schema(format = "uri"))]
    pub url: String,
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub size: i64,
    #[cfg_attr(feature = "openapi", schema(pattern = "^[0-9a-f]{64}$"))]
    pub blake3: String,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub expires_at: OffsetDateTime,
}

/// Everything a launcher needs to download and verify a release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReleaseDescriptor {
    pub package_id: Uuid,
    pub version_id: Uuid,
    pub platform: Platform,
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub sequence: i64,
    pub version_label: String,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub total_size: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub pack_count: i32,
    #[cfg_attr(feature = "openapi", schema(inline))]
    pub manifest: ManifestLink,
    pub signature: SignatureEnvelope,
    /// Withdrawn versions of this package/platform; launchers on them are offered this release.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub yanked_version_ids: Vec<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub published_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct DownloadUrlsRequest {
    #[cfg_attr(feature = "openapi", schema(min_items = 1, max_items = 500))]
    pub packs: std::collections::BTreeSet<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PackUrl {
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub pack_index: u32,
    #[cfg_attr(feature = "openapi", schema(format = "uri"))]
    pub url: String,
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub size: i64,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub expires_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PackUrlList {
    pub items: Vec<PackUrl>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct IntegrityReport {
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub pack_index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub chunk_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(max_length = 1000))]
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum CompatTarget {
    Linux,
    Macos,
}

impl CompatTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            CompatTarget::Linux => "linux",
            CompatTarget::Macos => "macos",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum CompatStatus {
    Verified,
    Playable,
    Unsupported,
    Untested,
}

/// A stored `vgames.compat/1` profile revision (docs/architecture/09-compatibility.md §4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SignedCompatProfile {
    #[cfg_attr(feature = "openapi", schema(inline))]
    pub target: CompatTarget,
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub revision: i64,
    #[cfg_attr(feature = "openapi", schema(inline))]
    pub status: CompatStatus,
    /// Standard base64 of the exact document bytes.
    pub document: String,
    pub signature: SignatureEnvelope,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CompatProfileList {
    pub items: Vec<SignedCompatProfile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CompatUpload {
    /// Standard base64 of the exact document bytes (≤ 64 KiB).
    pub document: String,
    pub signature: SignatureEnvelope,
}
