//! Builds the exact bytes of a `vgames.manifest/1` (02-package-format §5).
//!
//! Output is compact JSON with a fixed field order, so the same inputs always
//! give the same bytes (which are what the publisher signs). The execution
//! sections (`launch`, `controllers`, `saves`, `multiplayer`) are typed here
//! following 02 §5; the authoritative validation is
//! `vgames_core::manifest::parse_and_validate` (A5-T02), which the server runs on
//! finalize and the launcher runs before install.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::encode::Encoding;
use crate::layout::CHUNK_SIZE;
use crate::plan::{Packing, Plan};
use crate::source::Hashes;

pub const FORMAT: &str = "vgames.manifest/1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchTarget {
    pub id: String,
    pub label: String,
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub default: String,
    pub targets: Vec<LaunchTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Controllers {
    pub supported: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emulate_as: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveLocation {
    pub id: String,
    pub base: String,
    pub path: String,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saves {
    pub locations: Vec<SaveLocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Join {
    pub target: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Multiplayer {
    pub join: Join,
}

/// Execution-relevant package settings, provided by the publisher (for
/// example from a package spec file read by the CLI).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<Launch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controllers: Option<Controllers>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saves: Option<Saves>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiplayer: Option<Multiplayer>,
}

/// Identity of the version being published (from `POST …/versions`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionIdentity {
    pub server_id: Uuid,
    pub package_id: Uuid,
    pub version_id: Uuid,
    pub sequence: u64,
    pub version_label: String,
    pub platform: String,
    /// Unix seconds; written as RFC 3339 UTC.
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("hashes do not match the plan ({0})")]
    HashCount(&'static str),
    #[error("created_at is out of range")]
    Time,
    #[error("cannot serialize the manifest")]
    Json(String),
}

#[derive(Serialize)]
struct Totals {
    files: usize,
    bytes: u64,
    chunks: usize,
    packs: usize,
}

#[derive(Serialize)]
struct PackOut {
    size: u64,
    blake3: String,
}

#[derive(Serialize)]
struct ChunkOut {
    pack: u32,
    offset: u64,
    stored_size: u64,
    size: u64,
    encoding: Encoding,
    blake3: String,
}

#[derive(Serialize)]
struct FileOut<'a> {
    path: &'a str,
    size: u64,
    blake3: String,
    executable: bool,
    chunk: Option<u32>,
    offset: u64,
}

// Field order here is the byte order of the manifest. Do not reorder.
#[derive(Serialize)]
struct ManifestOut<'a> {
    format: &'static str,
    server_id: Uuid,
    package_id: Uuid,
    version_id: Uuid,
    sequence: u64,
    version_label: &'a str,
    platform: &'a str,
    created_at: String,
    chunk_size: u64,
    totals: Totals,
    packs: Vec<PackOut>,
    chunks: Vec<ChunkOut>,
    files: Vec<FileOut<'a>>,
    directories: &'a [String],
    #[serde(flatten)]
    execution: &'a Execution,
}

fn rfc3339(unix: i64) -> Result<String, ManifestError> {
    let t = time::OffsetDateTime::from_unix_timestamp(unix).map_err(|_| ManifestError::Time)?;
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    ))
}

/// Builds the manifest bytes.
pub fn build(
    identity: &VersionIdentity,
    execution: &Execution,
    plan: &Plan,
    packing: &Packing,
    hashes: &Hashes,
) -> Result<Vec<u8>, ManifestError> {
    if hashes.chunks.len() != packing.chunks.len() {
        return Err(ManifestError::HashCount("chunks"));
    }
    if hashes.files.len() != plan.files().len() {
        return Err(ManifestError::HashCount("files"));
    }
    if hashes.packs.len() != packing.packs.len() {
        return Err(ManifestError::HashCount("packs"));
    }
    let out = ManifestOut {
        format: FORMAT,
        server_id: identity.server_id,
        package_id: identity.package_id,
        version_id: identity.version_id,
        sequence: identity.sequence,
        version_label: &identity.version_label,
        platform: &identity.platform,
        created_at: rfc3339(identity.created_at)?,
        chunk_size: CHUNK_SIZE,
        totals: Totals {
            files: plan.files().len(),
            bytes: plan.total_bytes(),
            chunks: packing.chunks.len(),
            packs: packing.packs.len(),
        },
        packs: packing
            .packs
            .iter()
            .zip(&hashes.packs)
            .map(|(p, h)| PackOut {
                size: p.size,
                blake3: hex::encode(h),
            })
            .collect(),
        chunks: packing
            .chunks
            .iter()
            .zip(&plan.layout().chunks)
            .zip(&hashes.chunks)
            .map(|((c, slot), h)| ChunkOut {
                pack: c.placement.pack,
                offset: c.placement.offset,
                stored_size: c.stored_size,
                size: slot.size,
                encoding: c.encoding,
                blake3: hex::encode(h),
            })
            .collect(),
        files: plan
            .files()
            .iter()
            .zip(&plan.layout().files)
            .zip(&hashes.files)
            .map(|((f, slot), h)| FileOut {
                path: &f.path,
                size: f.size,
                blake3: hex::encode(h),
                executable: f.executable,
                chunk: slot.chunk,
                offset: slot.offset,
            })
            .collect(),
        directories: plan.directories(),
        execution,
    };
    serde_json::to_vec(&out).map_err(|e| ManifestError::Json(e.to_string()))
}
