//! `/.well-known/vgames.json` and `/v1/health`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Server identity, pinned by launchers when a user adds the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ServerInfo {
    /// Always `vgames.server/1`.
    pub format: String,
    pub server_id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motd: Option<String>,
    pub api_versions: Vec<ApiVersion>,
    /// Standard base64 of the 32-byte Ed25519 root public key.
    pub root_public_key: String,
    /// `VG1-XXXX-…` (docs/architecture/01-security.md §3.1).
    pub root_key_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_mode: Option<RegistrationMode>,
    pub features: Vec<Feature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_launcher_version: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ApiVersion {
    #[serde(rename = "v1")]
    V1,
}

/// Optional capabilities a server advertises.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    CloudSaves,
    Social,
    Messaging,
    Invites,
    AdminWeb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RegistrationMode {
    Open,
    Allowlist,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Ok,
    Degraded,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum DependencyStatus {
    Ok,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Health {
    pub status: HealthStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db: Option<DependencyStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<DependencyStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}
