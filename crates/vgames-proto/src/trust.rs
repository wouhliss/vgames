//! Trust bundles and publisher keys (docs/architecture/01-security.md §3.2).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::auth::UserPublic;

/// A `vgames.trust/1` bundle as uploaded and served: exact bytes plus the root signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SignedTrustBundle {
    /// Standard base64 of the exact bundle bytes.
    pub bundle: String,
    /// Standard base64 of the Ed25519 root signature.
    pub signature: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TrustBundleStored {
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub version: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublisherKey {
    #[cfg_attr(feature = "openapi", schema(pattern = "^[0-9a-f]{32}$"))]
    pub key_id: String,
    /// Standard base64 of the 32-byte Ed25519 public key.
    pub public_key: String,
    pub holder: UserPublic,
    pub label: String,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub not_before: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = DateTime))]
    pub not_after: OffsetDateTime,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = DateTime))]
    pub revoked_at: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublisherKeyList {
    /// 0 while no bundle has been uploaded.
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub bundle_version: i64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = DateTime))]
    pub bundle_expires_at: Option<OffsetDateTime>,
    pub items: Vec<PublisherKey>,
}
