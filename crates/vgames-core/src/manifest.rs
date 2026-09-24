//! `vgames.manifest/1` (02-package-format §5): types, parsing and the complete
//! validation run by the server on finalize and by the launcher before install.
//!
//! [`parse_and_validate`] takes the **exact signed bytes**. Verify the
//! signature over those bytes first (`crate::verify::verify_manifest` does
//! both, in the order of 01-security §3.4); never re-serialize a manifest to
//! verify or store it.

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::codec::{Digest, Timestamp};
use crate::layout::{self, CHUNK_SIZE, MAX_STORED_OVERHEAD, PACK_SIZE};
use crate::paths::{self, PathError};

pub const FORMAT: &str = "vgames.manifest/1";
/// Largest accepted manifest document.
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024 * 1024;
/// Limit on the summed byte length of a target's (or join's) `args`.
pub const MAX_ARGS_BYTES: usize = 64 * 1024;
pub const MAX_LAUNCH_TARGETS: usize = 64;
pub const MAX_SAVE_LOCATIONS: usize = 64;
pub const MAX_SAVE_PATTERNS: usize = 256;
pub const MAX_ENV_VARS: usize = 256;
pub const MAX_ENV_VALUE_BYTES: usize = 32 * 1024;
pub const MAX_LABEL_CHARS: usize = 64;
pub const MAX_ID_BYTES: usize = 64;
/// The placeholder replaced by a validated join secret (01-security §7).
pub const JOIN_SECRET_PLACEHOLDER: &str = "{join_secret}";

/// BLAKE3 of zero bytes: the only valid hash of an empty file.
pub const EMPTY_BLAKE3: Digest = Digest::from_bytes([
    0xaf, 0x13, 0x49, 0xb9, 0xf5, 0xf9, 0xa1, 0xa6, 0xa0, 0x40, 0x4d, 0xea, 0x36, 0xdc, 0xc9, 0x49,
    0x9b, 0xcb, 0x25, 0xc9, 0xad, 0xc1, 0x12, 0xb7, 0xcc, 0x9a, 0x93, 0xca, 0xe4, 0x1f, 0x32, 0x62,
]);

// ---------------------------------------------------------------- types

/// Build targets (`Platform` in the OpenAPI contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Platform {
    #[serde(rename = "windows-x86_64")]
    WindowsX86_64,
    #[serde(rename = "windows-aarch64")]
    WindowsAarch64,
    #[serde(rename = "linux-x86_64")]
    LinuxX86_64,
    #[serde(rename = "linux-aarch64")]
    LinuxAarch64,
    #[serde(rename = "macos-aarch64")]
    MacosAarch64,
    #[serde(rename = "macos-x86_64")]
    MacosX86_64,
}

impl Platform {
    pub const ALL: [Platform; 6] = [
        Platform::WindowsX86_64,
        Platform::WindowsAarch64,
        Platform::LinuxX86_64,
        Platform::LinuxAarch64,
        Platform::MacosAarch64,
        Platform::MacosX86_64,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Platform::WindowsX86_64 => "windows-x86_64",
            Platform::WindowsAarch64 => "windows-aarch64",
            Platform::LinuxX86_64 => "linux-x86_64",
            Platform::LinuxAarch64 => "linux-aarch64",
            Platform::MacosAarch64 => "macos-aarch64",
            Platform::MacosX86_64 => "macos-x86_64",
        }
    }
}

impl std::str::FromStr for Platform {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        Platform::ALL
            .into_iter()
            .find(|p| p.as_str() == s)
            .ok_or(())
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    Raw,
    Zstd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Totals {
    pub files: u64,
    pub bytes: u64,
    pub chunks: u64,
    pub packs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pack {
    pub size: u64,
    pub blake3: Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    pub pack: u32,
    pub offset: u64,
    pub stored_size: u64,
    pub size: u64,
    pub encoding: Encoding,
    pub blake3: Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub path: String,
    pub size: u64,
    pub blake3: Digest,
    pub executable: bool,
    pub chunk: Option<u32>,
    pub offset: u64,
}

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
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "unique_map"
    )]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub default: String,
    pub targets: Vec<LaunchTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerKind {
    Xinput,
    Dualshock4,
    Dualsense,
    SwitchPro,
    Generic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmulatedController {
    Xbox360,
    Dualshock4,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Controllers {
    pub supported: Vec<ControllerKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emulate_as: Option<EmulatedController>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveBase {
    Install,
    Home,
    Documents,
    SavedGames,
    Appdata,
    Localappdata,
    XdgData,
    XdgConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveLocation {
    pub id: String,
    pub base: SaveBase,
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

/// A parsed `vgames.manifest/1`. Only [`parse_and_validate`] produces values
/// that are known to satisfy every rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    #[serde(deserialize_with = "strict_uuid")]
    pub server_id: Uuid,
    #[serde(deserialize_with = "strict_uuid")]
    pub package_id: Uuid,
    #[serde(deserialize_with = "strict_uuid")]
    pub version_id: Uuid,
    pub sequence: u64,
    pub version_label: String,
    pub platform: Platform,
    pub created_at: Timestamp,
    pub chunk_size: u64,
    pub totals: Totals,
    pub packs: Vec<Pack>,
    pub chunks: Vec<Chunk>,
    pub files: Vec<File>,
    #[serde(default)]
    pub directories: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<Launch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controllers: Option<Controllers>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saves: Option<Saves>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiplayer: Option<Multiplayer>,
}

/// UUIDs only in the canonical lowercase hyphenated form (36 characters).
pub(crate) fn strict_uuid<'de, D: Deserializer<'de>>(d: D) -> Result<Uuid, D::Error> {
    let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
    parse_strict_uuid(&s)
        .ok_or_else(|| serde::de::Error::custom("expected a lowercase hyphenated UUID"))
}

pub(crate) fn parse_strict_uuid(s: &str) -> Option<Uuid> {
    if s.len() != 36 {
        return None;
    }
    let u = Uuid::try_parse(s).ok()?;
    (u.hyphenated().to_string() == s && !u.is_nil()).then_some(u)
}

/// A string map that refuses duplicate keys (serde_json would keep the last one).
pub(crate) fn unique_map<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a map of strings without duplicate keys")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut a: A,
        ) -> Result<Self::Value, A::Error> {
            let mut m = BTreeMap::new();
            while let Some((k, v)) = a.next_entry::<String, String>()? {
                if m.contains_key(&k) {
                    return Err(serde::de::Error::custom(format_args!(
                        "duplicate key {:?}",
                        k.chars().take(64).collect::<String>()
                    )));
                }
                m.insert(k, v);
            }
            Ok(m)
        }
    }
    d.deserialize_map(V)
}

// ---------------------------------------------------------------- errors

/// Why a manifest was refused. Indices are positions in the named array.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest is {size} bytes, more than {MAX_MANIFEST_BYTES}")]
    TooLarge { size: usize },
    #[error("manifest is not valid vgames.manifest/1 JSON: {0}")]
    Json(String),
    #[error("unknown manifest format {0:?}")]
    Format(String),
    #[error("sequence must be greater than 0")]
    ZeroSequence,
    #[error("version_label must be 1 to {MAX_LABEL_CHARS} characters without control characters")]
    VersionLabel,
    #[error("chunk_size must be {CHUNK_SIZE}, found {0}")]
    ChunkSize(u64),
    #[error("totals.{field} is {declared}, actual {actual}")]
    Totals {
        field: &'static str,
        declared: u64,
        actual: u64,
    },
    #[error("chunks[{index}]: {fault}")]
    Chunk { index: usize, fault: ChunkFault },
    #[error("packs[{index}]: {fault}")]
    Pack { index: usize, fault: PackFault },
    #[error("files[{index}] ({path:?}): {fault}")]
    File {
        index: usize,
        path: String,
        fault: FileFault,
    },
    #[error("invalid path: {0}")]
    Path(#[from] PathError),
    #[error("launch: {0}")]
    Launch(LaunchFault),
    #[error("launch.targets[{index}]: {fault}")]
    LaunchTarget { index: usize, fault: LaunchFault },
    #[error("launch.targets[{index}].env: {fault}")]
    Env { index: usize, fault: EnvFault },
    #[error("saves.locations[{index}]: {fault}")]
    Save { index: usize, fault: SaveFault },
    #[error("controllers.supported lists {0:?} twice")]
    DuplicateController(ControllerKind),
    #[error("multiplayer.join: {0}")]
    Join(JoinFault),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChunkFault {
    #[error("pack index {0} does not exist")]
    NoSuchPack(u32),
    #[error("chunks are not laid out in pack order")]
    PackOrder,
    #[error("offset {found} should be {expected} (chunks are contiguous in their pack)")]
    Offset { expected: u64, found: u64 },
    #[error("size {0} is 0 or larger than the chunk size")]
    Size(u64),
    #[error("stored_size {stored} is 0 or exceeds size + 64 KiB ({size})")]
    StoredSize { stored: u64, size: u64 },
    #[error("raw chunk must have stored_size == size")]
    RawStoredSize,
    #[error("size {found} does not match the layout ({expected})")]
    LayoutSize { expected: u64, found: u64 },
    #[error("size overflows")]
    Overflow,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PackFault {
    #[error("size {declared} does not equal the stored bytes of its chunks ({actual})")]
    Size { declared: u64, actual: u64 },
    #[error("size {0} exceeds 256 MiB")]
    TooLarge(u64),
    #[error("pack holds no chunk")]
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileFault {
    #[error("files are not sorted byte-wise by path")]
    Order,
    #[error("chunk/offset {found:?} do not match the layout {expected:?}")]
    Layout {
        expected: (Option<u32>, u64),
        found: (Option<u32>, u64),
    },
    #[error("an empty file must have no chunk and the BLAKE3 of zero bytes")]
    Empty,
    #[error("the layout of these files needs more chunks than the manifest lists")]
    TooManyChunks,
    #[error("total size overflows")]
    Overflow,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaunchFault {
    #[error("no launch targets")]
    NoTargets,
    #[error("more than {MAX_LAUNCH_TARGETS} launch targets")]
    TooManyTargets,
    #[error("default {0:?} is not a target id")]
    UnknownDefault(String),
    #[error("id must be 1 to {MAX_ID_BYTES} bytes without control characters")]
    Id,
    #[error("id {0:?} is used twice")]
    DuplicateId(String),
    #[error("label must be 1 to {MAX_LABEL_CHARS} characters without control characters")]
    Label,
    #[error("executable {0:?} is not a listed file")]
    Executable(String),
    #[error("working_dir {0:?} is not a directory of the package")]
    WorkingDir(String),
    #[error("args exceed {MAX_ARGS_BYTES} bytes or contain a NUL character")]
    Args,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvFault {
    #[error("more than {MAX_ENV_VARS} variables")]
    TooMany,
    #[error("{0:?} is not a valid variable name (^[A-Z_][A-Z0-9_]{{0,63}}$)")]
    KeySyntax(String),
    #[error("{0:?} may not be set by a package")]
    KeyDenied(String),
    #[error("the value of {0:?} is longer than {MAX_ENV_VALUE_BYTES} bytes or contains NUL")]
    Value(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SaveFault {
    #[error("more than {MAX_SAVE_LOCATIONS} save locations")]
    TooMany,
    #[error("id must be 1 to {MAX_ID_BYTES} bytes without control characters")]
    Id,
    #[error("id {0:?} is used twice")]
    DuplicateId(String),
    #[error("path: {0}")]
    Path(PathError),
    #[error("pattern {0:?} is empty, absolute, escapes the location, or is too long")]
    Pattern(String),
    #[error("more than {MAX_SAVE_PATTERNS} include/exclude patterns")]
    TooManyPatterns,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JoinFault {
    #[error("target {0:?} is not a launch target")]
    UnknownTarget(String),
    #[error("{{join_secret}} may only appear as a whole argument")]
    PartialPlaceholder,
    #[error("args exceed {MAX_ARGS_BYTES} bytes or contain a NUL character")]
    Args,
}

// ---------------------------------------------------------------- validation

/// Parses the exact manifest bytes and applies every rule of 02 §5.
pub fn parse_and_validate(bytes: &[u8]) -> Result<Manifest, ManifestError> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::TooLarge { size: bytes.len() });
    }
    let manifest: Manifest =
        serde_json::from_slice(bytes).map_err(|e| ManifestError::Json(e.to_string()))?;
    manifest.validate()?;
    Ok(manifest)
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

fn valid_label(s: &str) -> bool {
    let n = s.chars().count();
    (1..=MAX_LABEL_CHARS).contains(&n) && !has_control(s)
}

fn valid_id(s: &str) -> bool {
    (1..=MAX_ID_BYTES).contains(&s.len()) && !has_control(s)
}

fn args_ok(args: &[String]) -> bool {
    let total = args
        .iter()
        .try_fold(0usize, |acc, a| acc.checked_add(a.len()));
    total.is_some_and(|t| t <= MAX_ARGS_BYTES) && !args.iter().any(|a| a.contains('\0'))
}

/// Environment variables a package (or compat profile) may never set.
pub fn is_denied_env_key(key: &str) -> bool {
    matches!(
        key,
        "PATH" | "LD_PRELOAD" | "LD_LIBRARY_PATH" | "PYTHONPATH" | "COMSPEC"
    ) || key.starts_with("DYLD_")
}

/// `^[A-Z_][A-Z0-9_]{0,63}$`.
pub fn is_valid_env_key(key: &str) -> bool {
    let b = key.as_bytes();
    (1..=64).contains(&b.len())
        && b.first()
            .is_some_and(|c| c.is_ascii_uppercase() || *c == b'_')
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

/// Validates an environment map with the manifest rules plus `extra_denied`.
pub(crate) fn check_env(
    env: &BTreeMap<String, String>,
    extra_denied: impl Fn(&str) -> bool,
) -> Result<(), EnvFault> {
    if env.len() > MAX_ENV_VARS {
        return Err(EnvFault::TooMany);
    }
    for (k, v) in env {
        let short = || k.chars().take(64).collect::<String>();
        if !is_valid_env_key(k) {
            return Err(EnvFault::KeySyntax(short()));
        }
        if is_denied_env_key(k) || extra_denied(k) {
            return Err(EnvFault::KeyDenied(short()));
        }
        if v.len() > MAX_ENV_VALUE_BYTES || v.contains('\0') {
            return Err(EnvFault::Value(short()));
        }
    }
    Ok(())
}

/// A save include/exclude glob: relative, `/`-separated, no `..`, bounded.
fn valid_pattern(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= paths::MAX_PATH_BYTES
        && !p.starts_with('/')
        && !p.contains('\\')
        && !has_control(p)
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

impl Manifest {
    /// Applies every rule of 02 §5 to an already parsed manifest.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.format != FORMAT {
            return Err(ManifestError::Format(
                self.format.chars().take(64).collect(),
            ));
        }
        if self.sequence == 0 {
            return Err(ManifestError::ZeroSequence);
        }
        if !valid_label(&self.version_label) {
            return Err(ManifestError::VersionLabel);
        }
        if self.chunk_size != CHUNK_SIZE {
            return Err(ManifestError::ChunkSize(self.chunk_size));
        }
        self.check_totals()?;
        self.check_chunks_and_packs()?;
        self.check_files()?;
        paths::validate_tree(
            self.files.iter().map(|f| f.path.as_str()),
            self.directories.iter().map(String::as_str),
        )?;
        self.check_launch()?;
        self.check_controllers()?;
        self.check_saves()?;
        self.check_join()?;
        Ok(())
    }

    fn check_totals(&self) -> Result<(), ManifestError> {
        let bytes = self
            .files
            .iter()
            .try_fold(0u64, |acc, f| acc.checked_add(f.size))
            .ok_or(ManifestError::Totals {
                field: "bytes",
                declared: self.totals.bytes,
                actual: u64::MAX,
            })?;
        for (field, declared, actual) in [
            ("files", self.totals.files, self.files.len() as u64),
            ("bytes", self.totals.bytes, bytes),
            ("chunks", self.totals.chunks, self.chunks.len() as u64),
            ("packs", self.totals.packs, self.packs.len() as u64),
        ] {
            if declared != actual {
                return Err(ManifestError::Totals {
                    field,
                    declared,
                    actual,
                });
            }
        }
        Ok(())
    }

    fn check_chunks_and_packs(&self) -> Result<(), ManifestError> {
        let mut pack_bytes = vec![0u64; self.packs.len()];
        let mut current: Option<u32> = None;
        let mut next_offset = 0u64;
        for (index, c) in self.chunks.iter().enumerate() {
            let fault = |fault| ManifestError::Chunk { index, fault };
            if c.size == 0 || c.size > CHUNK_SIZE {
                return Err(fault(ChunkFault::Size(c.size)));
            }
            if c.stored_size == 0 || c.stored_size > c.size + MAX_STORED_OVERHEAD {
                return Err(fault(ChunkFault::StoredSize {
                    stored: c.stored_size,
                    size: c.size,
                }));
            }
            if c.encoding == Encoding::Raw && c.stored_size != c.size {
                return Err(fault(ChunkFault::RawStoredSize));
            }
            let sum = pack_bytes
                .get_mut(c.pack as usize)
                .ok_or(fault(ChunkFault::NoSuchPack(c.pack)))?;
            // Packs are filled in order: stay in the current pack or move to the next one.
            let same_pack = current == Some(c.pack);
            let next_pack = current.map_or(0, |p| p.saturating_add(1)) == c.pack;
            if same_pack {
                if c.offset != next_offset {
                    return Err(fault(ChunkFault::Offset {
                        expected: next_offset,
                        found: c.offset,
                    }));
                }
            } else if next_pack {
                if c.offset != 0 {
                    return Err(fault(ChunkFault::Offset {
                        expected: 0,
                        found: c.offset,
                    }));
                }
                current = Some(c.pack);
            } else {
                return Err(fault(ChunkFault::PackOrder));
            }
            next_offset = c
                .offset
                .checked_add(c.stored_size)
                .ok_or(fault(ChunkFault::Overflow))?;
            *sum = next_offset;
        }
        for (index, (p, actual)) in self.packs.iter().zip(pack_bytes).enumerate() {
            let fault = |fault| ManifestError::Pack { index, fault };
            if actual == 0 {
                return Err(fault(PackFault::Empty));
            }
            if p.size != actual {
                return Err(fault(PackFault::Size {
                    declared: p.size,
                    actual,
                }));
            }
            if p.size > PACK_SIZE {
                return Err(fault(PackFault::TooLarge(p.size)));
            }
        }
        Ok(())
    }

    fn check_files(&self) -> Result<(), ManifestError> {
        let file_fault = |index: usize, fault| ManifestError::File {
            index,
            path: self
                .files
                .get(index)
                .map(|f| f.path.chars().take(128).collect())
                .unwrap_or_default(),
            fault,
        };
        for (index, pair) in self.files.windows(2).enumerate() {
            if let [a, b] = pair
                && a.path.as_bytes() >= b.path.as_bytes()
            {
                return Err(file_fault(index + 1, FileFault::Order));
            }
        }
        let sizes: Vec<u64> = self.files.iter().map(|f| f.size).collect();
        let layout = layout::assign_chunks_bounded(&sizes, CHUNK_SIZE, self.chunks.len() as u64)
            .map_err(|e| match e {
                layout::LayoutError::Overflow => file_fault(0, FileFault::Overflow),
                _ => file_fault(0, FileFault::TooManyChunks),
            })?;
        if layout.chunks.len() != self.chunks.len() {
            return Err(ManifestError::Totals {
                field: "chunks",
                declared: self.chunks.len() as u64,
                actual: layout.chunks.len() as u64,
            });
        }
        for (index, (f, slot)) in self.files.iter().zip(&layout.files).enumerate() {
            if f.size == 0 && (f.chunk.is_some() || f.offset != 0 || f.blake3 != EMPTY_BLAKE3) {
                return Err(file_fault(index, FileFault::Empty));
            }
            if (f.chunk, f.offset) != (slot.chunk, slot.offset) {
                return Err(file_fault(
                    index,
                    FileFault::Layout {
                        expected: (slot.chunk, slot.offset),
                        found: (f.chunk, f.offset),
                    },
                ));
            }
        }
        for (index, (c, slot)) in self.chunks.iter().zip(&layout.chunks).enumerate() {
            if c.size != slot.size {
                return Err(ManifestError::Chunk {
                    index,
                    fault: ChunkFault::LayoutSize {
                        expected: slot.size,
                        found: c.size,
                    },
                });
            }
        }
        Ok(())
    }

    /// Every directory of the package: listed ones and parents of files.
    fn directory_set(&self) -> HashSet<&str> {
        let mut dirs: HashSet<&str> = self.directories.iter().map(String::as_str).collect();
        for f in &self.files {
            let mut p = f.path.as_str();
            while let Some((parent, _)) = p.rsplit_once('/') {
                if !dirs.insert(parent) {
                    break;
                }
                p = parent;
            }
        }
        dirs
    }

    fn check_launch(&self) -> Result<(), ManifestError> {
        let Some(launch) = &self.launch else {
            return Ok(());
        };
        if launch.targets.is_empty() {
            return Err(ManifestError::Launch(LaunchFault::NoTargets));
        }
        if launch.targets.len() > MAX_LAUNCH_TARGETS {
            return Err(ManifestError::Launch(LaunchFault::TooManyTargets));
        }
        let files: HashSet<&str> = self.files.iter().map(|f| f.path.as_str()).collect();
        let dirs = self.directory_set();
        let mut ids = HashSet::new();
        for (index, t) in launch.targets.iter().enumerate() {
            let fault = |fault| ManifestError::LaunchTarget { index, fault };
            if !valid_id(&t.id) {
                return Err(fault(LaunchFault::Id));
            }
            if !ids.insert(t.id.as_str()) {
                return Err(fault(LaunchFault::DuplicateId(t.id.clone())));
            }
            if !valid_label(&t.label) {
                return Err(fault(LaunchFault::Label));
            }
            if !files.contains(t.executable.as_str()) {
                return Err(fault(LaunchFault::Executable(
                    t.executable.chars().take(128).collect(),
                )));
            }
            if let Some(wd) = &t.working_dir
                && !dirs.contains(wd.as_str())
            {
                return Err(fault(LaunchFault::WorkingDir(
                    wd.chars().take(128).collect(),
                )));
            }
            if !args_ok(&t.args) {
                return Err(fault(LaunchFault::Args));
            }
            check_env(&t.env, |_| false).map_err(|fault| ManifestError::Env { index, fault })?;
        }
        if !ids.contains(launch.default.as_str()) {
            return Err(ManifestError::Launch(LaunchFault::UnknownDefault(
                launch.default.chars().take(64).collect(),
            )));
        }
        Ok(())
    }

    fn check_controllers(&self) -> Result<(), ManifestError> {
        let Some(c) = &self.controllers else {
            return Ok(());
        };
        let mut seen = HashSet::new();
        for kind in &c.supported {
            if !seen.insert(*kind) {
                return Err(ManifestError::DuplicateController(*kind));
            }
        }
        Ok(())
    }

    fn check_saves(&self) -> Result<(), ManifestError> {
        let Some(saves) = &self.saves else {
            return Ok(());
        };
        if saves.locations.len() > MAX_SAVE_LOCATIONS {
            return Err(ManifestError::Save {
                index: MAX_SAVE_LOCATIONS,
                fault: SaveFault::TooMany,
            });
        }
        let mut ids = HashSet::new();
        for (index, loc) in saves.locations.iter().enumerate() {
            let fault = |fault| ManifestError::Save { index, fault };
            if !valid_id(&loc.id) {
                return Err(fault(SaveFault::Id));
            }
            if !ids.insert(loc.id.as_str()) {
                return Err(fault(SaveFault::DuplicateId(loc.id.clone())));
            }
            paths::validate_path(&loc.path).map_err(|e| fault(SaveFault::Path(e)))?;
            if loc.include.len() + loc.exclude.len() > MAX_SAVE_PATTERNS {
                return Err(fault(SaveFault::TooManyPatterns));
            }
            if let Some(bad) = loc
                .include
                .iter()
                .chain(&loc.exclude)
                .find(|p| !valid_pattern(p))
            {
                return Err(fault(SaveFault::Pattern(bad.chars().take(128).collect())));
            }
        }
        Ok(())
    }

    fn check_join(&self) -> Result<(), ManifestError> {
        let Some(mp) = &self.multiplayer else {
            return Ok(());
        };
        let join = &mp.join;
        let known = self
            .launch
            .as_ref()
            .is_some_and(|l| l.targets.iter().any(|t| t.id == join.target));
        if !known {
            return Err(ManifestError::Join(JoinFault::UnknownTarget(
                join.target.chars().take(64).collect(),
            )));
        }
        if join
            .args
            .iter()
            .any(|a| a.contains(JOIN_SECRET_PLACEHOLDER) && a != JOIN_SECRET_PLACEHOLDER)
        {
            return Err(ManifestError::Join(JoinFault::PartialPlaceholder));
        }
        if !args_ok(&join.args) {
            return Err(ManifestError::Join(JoinFault::Args));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
