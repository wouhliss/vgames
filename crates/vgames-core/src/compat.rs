//! `vgames.compat/1` compatibility profiles (09-compatibility §4): how a
//! Windows build runs on Linux (Proton via umu) or macOS (Wine). Profiles change
//! execution, so they are signed like manifests (context `vgames/compat/v1`)
//! and verified with [`crate::verify::verify_compat_profile`].

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::codec::Timestamp;
use crate::manifest::{self, EnvFault, Platform, strict_uuid, unique_map};

pub const FORMAT: &str = "vgames.compat/1";
pub const MAX_PROFILE_BYTES: usize = 64 * 1024;
pub const MAX_NOTES_CHARS: usize = 2000;
pub const MAX_PREFER: usize = 8;
pub const MAX_DLL_OVERRIDES: usize = 64;
pub const MAX_WINETRICKS: usize = 32;

/// Winetricks verbs a profile may install (09 §4). Only runtimes, codecs and
/// fonts from Microsoft redistributables: nothing that replaces graphics
/// layers (those come from the signed runtime catalog) or changes Wine itself.
pub const WINETRICKS_ALLOWLIST: &[&str] = &[
    "amstream",
    "corefonts",
    "d3dcompiler_42",
    "d3dcompiler_43",
    "d3dcompiler_46",
    "d3dcompiler_47",
    "d3dx10",
    "d3dx10_43",
    "d3dx11_42",
    "d3dx11_43",
    "d3dx9",
    "d3dx9_43",
    "devenum",
    "dinput8",
    "directplay",
    "dotnet20",
    "dotnet35",
    "dotnet40",
    "dotnet45",
    "dotnet452",
    "dotnet46",
    "dotnet461",
    "dotnet462",
    "dotnet472",
    "dotnet48",
    "dotnetdesktop6",
    "dotnetdesktop7",
    "dotnetdesktop8",
    "faudio",
    "gdiplus",
    "mfc140",
    "mfc42",
    "msxml3",
    "msxml6",
    "openal",
    "physx",
    "quartz",
    "vb6run",
    "vcrun2005",
    "vcrun2008",
    "vcrun2010",
    "vcrun2012",
    "vcrun2013",
    "vcrun2015",
    "vcrun2017",
    "vcrun2019",
    "vcrun2022",
    "wmp9",
    "xact",
    "xact_x64",
    "xna31",
    "xna40",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Linux,
    Macos,
}

impl Target {
    pub const fn as_str(self) -> &'static str {
        match self {
            Target::Linux => "linux",
            Target::Macos => "macos",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Verified,
    Playable,
    Unsupported,
    Untested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunnerKind {
    /// Linux: Proton through umu-launcher.
    Proton,
    /// macOS: WineHQ builds with a Metal graphics backend.
    Wine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Graphics {
    D3dmetal,
    Dxmt,
    Dxvk,
    Wined3d,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppliesTo {
    pub platform: Platform,
    pub min_sequence: u64,
    #[serde(default)]
    pub max_sequence: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runner {
    pub kind: RunnerKind,
    /// Runtime catalog ids, most preferred first (e.g. `umu-proton`, `ge-proton`).
    #[serde(default)]
    pub prefer: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub umu_game_id: Option<String>,
    /// macOS only, most preferred first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graphics: Vec<Graphics>,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "unique_map"
    )]
    pub env: BTreeMap<String, String>,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "unique_map"
    )]
    pub dll_overrides: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub winetricks: Vec<String>,
}

/// A parsed `vgames.compat/1` profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatProfile {
    pub format: String,
    #[serde(deserialize_with = "strict_uuid")]
    pub server_id: Uuid,
    #[serde(deserialize_with = "strict_uuid")]
    pub package_id: Uuid,
    pub target: Target,
    pub revision: u64,
    pub created_at: Timestamp,
    pub applies_to: AppliesTo,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub runner: Runner,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompatError {
    #[error("profile is larger than {MAX_PROFILE_BYTES} bytes")]
    TooLarge,
    #[error("not valid vgames.compat/1 JSON: {0}")]
    Json(String),
    #[error("unknown profile format {0:?}")]
    Format(String),
    #[error("revision must be at least 1")]
    ZeroRevision,
    #[error("applies_to.platform must be a Windows build")]
    NotWindows,
    #[error("applies_to sequence range is empty or starts at 0")]
    SequenceRange,
    #[error("notes must be at most {MAX_NOTES_CHARS} characters of plain text")]
    Notes,
    #[error("runner.kind must be proton for linux and wine for macos")]
    RunnerKind,
    #[error(
        "runner.prefer: {0:?} is not a runtime id, is repeated, or there are more than {MAX_PREFER}"
    )]
    Prefer(String),
    #[error("runner.min_version {0:?} is not a version string")]
    MinVersion(String),
    #[error("runner.umu_game_id {0:?} is not a umu id (umu-…)")]
    UmuGameId(String),
    #[error("runner.graphics is for macos only and must not repeat a backend")]
    Graphics,
    #[error("runner.env: {0}")]
    Env(EnvFault),
    #[error("runner.dll_overrides: {0:?} is not a valid DLL name or mode")]
    DllOverride(String),
    #[error("runner.winetricks: {0:?} is not an allowed verb, or it is repeated")]
    Winetricks(String),
    #[error("too many dll_overrides or winetricks verbs")]
    TooMany,
}

/// Environment variables the launcher sets itself for compat launches; a
/// profile may not override them (09 §2, §3).
pub fn is_launcher_owned_env_key(key: &str) -> bool {
    matches!(
        key,
        "WINEPREFIX"
            | "WINEDLLOVERRIDES"
            | "WINEDLLPATH"
            | "WINEPATH"
            | "WINELOADER"
            | "WINESERVER"
            | "PROTONPATH"
            | "GAMEID"
            | "STORE"
    ) || key.starts_with("STEAM_COMPAT_")
        || key.starts_with("UMU_")
        || key.starts_with("PRESSURE_VESSEL_")
}

/// `^[a-z0-9][a-z0-9-]{0,63}$`: runtime catalog ids.
pub fn is_valid_runtime_id(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && b.first()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// DLL override names: `^[a-z0-9_.-]{1,64}$`.
pub fn is_valid_dll_name(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-')
        })
}

/// Wine load order: `""` (disabled) or one or two of `native`/`builtin`/`n`/`b`, comma-separated.
pub fn is_valid_dll_mode(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    let parts: Vec<&str> = s.split(',').collect();
    parts.len() <= 2
        && parts
            .iter()
            .all(|p| matches!(*p, "native" | "builtin" | "n" | "b"))
}

fn valid_version(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'+' | b'-'))
}

fn valid_umu_id(s: &str) -> bool {
    s.strip_prefix("umu-").is_some_and(|rest| {
        (1..=64).contains(&rest.len())
            && rest
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
    })
}

fn short(s: &str) -> String {
    s.chars().take(64).collect()
}

/// Parses exact profile bytes and applies every rule of 09 §4.
pub fn parse_and_validate(bytes: &[u8]) -> Result<CompatProfile, CompatError> {
    if bytes.len() > MAX_PROFILE_BYTES {
        return Err(CompatError::TooLarge);
    }
    let p: CompatProfile =
        serde_json::from_slice(bytes).map_err(|e| CompatError::Json(e.to_string()))?;
    p.validate()?;
    Ok(p)
}

impl CompatProfile {
    pub fn validate(&self) -> Result<(), CompatError> {
        if self.format != FORMAT {
            return Err(CompatError::Format(short(&self.format)));
        }
        if self.revision == 0 {
            return Err(CompatError::ZeroRevision);
        }
        if !matches!(
            self.applies_to.platform,
            Platform::WindowsX86_64 | Platform::WindowsAarch64
        ) {
            return Err(CompatError::NotWindows);
        }
        let a = &self.applies_to;
        if a.min_sequence == 0 || a.max_sequence.is_some_and(|max| max < a.min_sequence) {
            return Err(CompatError::SequenceRange);
        }
        if let Some(notes) = &self.notes {
            let plain = !notes
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t');
            if notes.chars().count() > MAX_NOTES_CHARS || !plain {
                return Err(CompatError::Notes);
            }
        }
        let r = &self.runner;
        match (self.target, r.kind) {
            (Target::Linux, RunnerKind::Proton) | (Target::Macos, RunnerKind::Wine) => {}
            _ => return Err(CompatError::RunnerKind),
        }
        let mut seen = HashSet::new();
        if r.prefer.len() > MAX_PREFER {
            return Err(CompatError::Prefer(String::new()));
        }
        for id in &r.prefer {
            if !is_valid_runtime_id(id) || !seen.insert(id.as_str()) {
                return Err(CompatError::Prefer(short(id)));
            }
        }
        if let Some(v) = &r.min_version
            && !valid_version(v)
        {
            return Err(CompatError::MinVersion(short(v)));
        }
        if let Some(id) = &r.umu_game_id
            && !valid_umu_id(id)
        {
            return Err(CompatError::UmuGameId(short(id)));
        }
        let mut backends = HashSet::new();
        if (self.target == Target::Linux && !r.graphics.is_empty())
            || !r.graphics.iter().all(|g| backends.insert(*g))
        {
            return Err(CompatError::Graphics);
        }
        manifest::check_env(&r.env, is_launcher_owned_env_key).map_err(CompatError::Env)?;
        if r.dll_overrides.len() > MAX_DLL_OVERRIDES || r.winetricks.len() > MAX_WINETRICKS {
            return Err(CompatError::TooMany);
        }
        for (name, mode) in &r.dll_overrides {
            if !is_valid_dll_name(name) || !is_valid_dll_mode(mode) {
                return Err(CompatError::DllOverride(short(name)));
            }
        }
        let mut verbs = HashSet::new();
        for verb in &r.winetricks {
            if !WINETRICKS_ALLOWLIST.contains(&verb.as_str()) || !verbs.insert(verb.as_str()) {
                return Err(CompatError::Winetricks(short(verb)));
            }
        }
        Ok(())
    }

    /// Whether this profile covers an installed build.
    pub fn applies_to(&self, platform: Platform, sequence: u64) -> bool {
        let a = &self.applies_to;
        a.platform == platform
            && sequence >= a.min_sequence
            && a.max_sequence.is_none_or(|max| sequence <= max)
    }

    /// `WINEDLLOVERRIDES` built from the validated overrides (`name=mode;…`).
    pub fn wine_dll_overrides(&self) -> String {
        self.runner
            .dll_overrides
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(";")
    }
}

#[cfg(test)]
pub(crate) mod tests;
