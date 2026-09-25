//! `vgames.runtimes/1`: the signed catalog of compatibility runtimes
//! (09-compatibility §5, A5-T12).
//!
//! Proton, umu-launcher, Wine, D3DMetal, DXMT, DXVK-macOS and MoltenVK are
//! third-party code the launcher downloads and runs. Nothing runs unless its
//! bytes are pinned here: every entry carries the SHA-256 and size of the
//! archive, and the catalog itself is signed with the vgames runtime-catalog
//! key (minisign, Ed25519; public key compiled into the launcher, separate from
//! the updater key).
//!
//! [`verify_catalog`] checks, in order: the minisign signature over the exact
//! bytes (prehashed signatures only), every format rule ([`parse_and_validate`]),
//! and the catalog version against the highest one seen (no rollback).

use serde::{Deserialize, Serialize};

use crate::codec::Timestamp;

pub const FORMAT: &str = "vgames.runtimes/1";
/// Largest `runtimes.json` accepted.
pub const MAX_CATALOG_BYTES: usize = 1024 * 1024;
/// Largest `runtimes.json.minisig` accepted.
pub const MAX_SIGNATURE_BYTES: usize = 4096;
pub const MAX_ENTRIES: usize = 2000;
/// Largest runtime archive (Proton builds are about 0.5 GiB).
pub const MAX_RUNTIME_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const URL_PREFIX: &str = "https://github.com/";
const MAX_URL_LEN: usize = 512;

/// Which runtime (09-compatibility §2, §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeId {
    UmuProton,
    GeProton,
    UmuLauncher,
    WineMacos,
    D3dmetal,
    Dxmt,
    DxvkMacos,
    Moltenvk,
}

impl RuntimeId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UmuProton => "umu-proton",
            Self::GeProton => "ge-proton",
            Self::UmuLauncher => "umu-launcher",
            Self::WineMacos => "wine-macos",
            Self::D3dmetal => "d3dmetal",
            Self::Dxmt => "dxmt",
            Self::DxvkMacos => "dxvk-macos",
            Self::Moltenvk => "moltenvk",
        }
    }

    /// The only OS the runtime is for.
    pub fn os(self) -> Os {
        match self {
            Self::UmuProton | Self::GeProton | Self::UmuLauncher => Os::Linux,
            Self::WineMacos | Self::D3dmetal | Self::Dxmt | Self::DxvkMacos | Self::Moltenvk => {
                Os::Macos
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Linux,
    Macos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    X86_64,
    /// Apple silicon and 64-bit ARM; `arm64` (Apple's name) is accepted on input.
    #[serde(alias = "arm64")]
    Aarch64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchiveFormat {
    #[serde(rename = "tar.gz")]
    TarGz,
    #[serde(rename = "tar.xz")]
    TarXz,
    #[serde(rename = "tar.zst")]
    TarZst,
    #[serde(rename = "zip")]
    Zip,
}

/// Redistribution terms narrower than the license's usual ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Redistribution {
    /// Only while vgames is non-commercial (D3DMetal, Apple's license).
    NonCommercial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    pub id: RuntimeId,
    /// Upstream version or tag, e.g. `GE-Proton10-17`, `1.2.3`.
    pub version: String,
    pub os: Os,
    pub arch: Arch,
    /// A GitHub release asset: `https://github.com/<owner>/<repo>/releases/download/<tag>/<file>`.
    pub url: String,
    /// SHA-256 of the archive, lowercase hex. Checked before extraction.
    pub sha256: String,
    pub size: u64,
    pub archive: ArchiveFormat,
    /// SPDX license expression, shown in Settings → Compatibility → Licenses.
    pub license: String,
    /// Oldest launcher that can use this entry (`x.y.z`).
    pub min_launcher_version: String,
    /// x86_64 macOS code that needs Rosetta 2 on Apple silicon.
    #[serde(default, skip_serializing_if = "is_false")]
    pub rosetta_required: bool,
    /// Last macOS major release this entry is known to work on (e.g. 27: Rosetta 2 shrinks after it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub macos_max_supported: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redistribution: Option<Redistribution>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub format: String,
    /// Strictly increasing with every published catalog.
    pub version: u64,
    pub generated_at: Timestamp,
    /// True if vgames were distributed commercially: then no `non-commercial` entry may exist
    /// (09-compatibility §7), and D3DMetal leaves the catalog.
    pub commercial: bool,
    pub runtimes: Vec<Runtime>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimesError {
    #[error("runtime catalog is larger than {MAX_CATALOG_BYTES} bytes")]
    TooLarge,
    #[error("the catalog signature is malformed: {0}")]
    SignatureMalformed(String),
    #[error("the catalog signature is not valid for the vgames runtime-catalog key")]
    BadSignature,
    #[error("the runtime-catalog public key is malformed")]
    PublicKeyMalformed,
    #[error("runtime catalog JSON is invalid: {0}")]
    Json(String),
    #[error("unsupported format {0:?} (expected {FORMAT})")]
    Format(String),
    #[error("catalog version must be at least 1")]
    VersionZero,
    #[error("catalog version {got} is older than {seen} (rollback)")]
    Rollback { got: u64, seen: u64 },
    #[error("more than {MAX_ENTRIES} runtimes")]
    TooManyEntries,
    #[error("runtimes[{index}] ({id}): {reason}")]
    Entry {
        index: usize,
        id: &'static str,
        reason: String,
    },
}

fn entry(index: usize, rt: &Runtime, reason: impl Into<String>) -> RuntimesError {
    RuntimesError::Entry {
        index,
        id: rt.id.as_str(),
        reason: reason.into(),
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_version_text(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
        && s.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
}

fn is_semver_core(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 9
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
        })
}

fn is_license(s: &str) -> bool {
    (1..=128).contains(&s.len())
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b' ' | b'(' | b')')
        })
}

/// A GitHub release asset URL: nothing that could point elsewhere after parsing.
fn is_release_url(s: &str) -> bool {
    let Some(path) = s.strip_prefix(URL_PREFIX) else {
        return false;
    };
    if s.len() > MAX_URL_LEN
        || !s.bytes().all(|b| b.is_ascii_graphic())
        || s.contains(['?', '#', '@', '\\', '%'])
    {
        return false;
    }
    let segments: Vec<&str> = path.split('/').collect();
    segments.len() == 6
        && segments
            .iter()
            .all(|seg| !seg.is_empty() && *seg != "." && *seg != "..")
        && segments.get(2) == Some(&"releases")
        && segments.get(3) == Some(&"download")
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn check_entry(index: usize, rt: &Runtime, commercial: bool) -> Result<(), RuntimesError> {
    if !is_version_text(&rt.version) {
        return Err(entry(
            index,
            rt,
            "version must be 1-64 of A-Z a-z 0-9 . _ + -",
        ));
    }
    if rt.os != rt.id.os() {
        return Err(entry(index, rt, "wrong os for this runtime"));
    }
    if !is_release_url(&rt.url) {
        return Err(entry(
            index,
            rt,
            "url must be a plain https://github.com/<owner>/<repo>/releases/download/<tag>/<file>",
        ));
    }
    if !is_sha256(&rt.sha256) {
        return Err(entry(
            index,
            rt,
            "sha256 must be 64 lowercase hex characters",
        ));
    }
    if rt.size == 0 || rt.size > MAX_RUNTIME_BYTES {
        return Err(entry(index, rt, "size must be between 1 byte and 8 GiB"));
    }
    if !is_license(&rt.license) {
        return Err(entry(index, rt, "license must be an SPDX expression"));
    }
    if !is_semver_core(&rt.min_launcher_version) {
        return Err(entry(index, rt, "min_launcher_version must be x.y.z"));
    }
    if rt.os != Os::Macos && (rt.rosetta_required || rt.macos_max_supported.is_some()) {
        return Err(entry(
            index,
            rt,
            "rosetta_required and macos_max_supported are macOS-only",
        ));
    }
    if rt.rosetta_required && rt.arch != Arch::X86_64 {
        return Err(entry(index, rt, "only x86_64 code needs Rosetta 2"));
    }
    if rt
        .macos_max_supported
        .is_some_and(|v| !(11..=99).contains(&v))
    {
        return Err(entry(
            index,
            rt,
            "macos_max_supported must be a macOS major version",
        ));
    }
    if rt.id == RuntimeId::D3dmetal {
        // Apple's license: Apple silicon only, unmodified, non-commercial redistribution.
        if rt.arch != Arch::Aarch64 {
            return Err(entry(
                index,
                rt,
                "D3DMetal is for Apple silicon (aarch64) only",
            ));
        }
        if rt.redistribution != Some(Redistribution::NonCommercial) {
            return Err(entry(
                index,
                rt,
                "D3DMetal must be marked redistribution = non-commercial",
            ));
        }
    }
    if commercial && rt.redistribution == Some(Redistribution::NonCommercial) {
        return Err(entry(
            index,
            rt,
            "a commercial catalog cannot carry non-commercial runtimes",
        ));
    }
    Ok(())
}

/// Parses `runtimes.json` and checks every rule. Call [`verify_catalog`] on bytes
/// from the network: this does not check the signature.
pub fn parse_and_validate(bytes: &[u8]) -> Result<Catalog, RuntimesError> {
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err(RuntimesError::TooLarge);
    }
    let catalog: Catalog =
        serde_json::from_slice(bytes).map_err(|e| RuntimesError::Json(e.to_string()))?;
    if catalog.format != FORMAT {
        return Err(RuntimesError::Format(catalog.format));
    }
    if catalog.version == 0 {
        return Err(RuntimesError::VersionZero);
    }
    if catalog.runtimes.len() > MAX_ENTRIES {
        return Err(RuntimesError::TooManyEntries);
    }
    let mut seen = std::collections::BTreeSet::new();
    for (index, rt) in catalog.runtimes.iter().enumerate() {
        check_entry(index, rt, catalog.commercial)?;
        if !seen.insert((rt.id, rt.version.as_str(), rt.os, rt.arch)) {
            return Err(entry(index, rt, "duplicate id + version + os + arch"));
        }
    }
    Ok(catalog)
}

/// A catalog whose signature, format and version all passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCatalog {
    pub catalog: Catalog,
    /// False when the version equals the highest one seen (a refresh).
    pub newer: bool,
}

/// Verifies `runtimes.json` (`bytes`, exact) against its minisign signature
/// (`minisig`, the `.minisig` file) and the runtime-catalog `public_key`
/// (the base64 line of the minisign public key, or the whole `.pub` file).
/// `last_seen_version`: the highest catalog version this launcher accepted.
pub fn verify_catalog(
    bytes: &[u8],
    minisig: &str,
    public_key: &str,
    last_seen_version: Option<u64>,
) -> Result<VerifiedCatalog, RuntimesError> {
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err(RuntimesError::TooLarge);
    }
    if minisig.len() > MAX_SIGNATURE_BYTES {
        return Err(RuntimesError::SignatureMalformed("too large".into()));
    }
    let key = public_key.trim();
    let pk = if key.contains('\n') {
        minisign_verify::PublicKey::decode(key)
    } else {
        minisign_verify::PublicKey::from_base64(key)
    }
    .map_err(|_| RuntimesError::PublicKeyMalformed)?;
    let signature = minisign_verify::Signature::decode(minisig)
        .map_err(|e| RuntimesError::SignatureMalformed(e.to_string()))?;
    // Prehashed (BLAKE2b) signatures only: legacy ones are refused.
    pk.verify(bytes, &signature, false)
        .map_err(|_| RuntimesError::BadSignature)?;
    let catalog = parse_and_validate(bytes)?;
    if let Some(seen) = last_seen_version
        && catalog.version < seen
    {
        return Err(RuntimesError::Rollback {
            got: catalog.version,
            seen,
        });
    }
    let newer = last_seen_version.is_none_or(|seen| catalog.version > seen);
    Ok(VerifiedCatalog { catalog, newer })
}

#[cfg(test)]
mod tests;
