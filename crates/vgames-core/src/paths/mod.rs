//! Package path rules (02-package-format §3). The packer refuses an invalid
//! tree, and the server and the launcher refuse a manifest with one invalid
//! path, all through this module.
//!
//! A path is valid only if all of these hold:
//! - UTF-8 in Unicode NFC (rejected, never normalized).
//! - Relative and `/`-separated: no leading `/`, no `\`, no empty, `.` or `..` component.
//! - ≤ 512 bytes, ≤ 255 bytes per component, ≤ 64 components.
//! - No control characters (U+0000–U+001F, U+007F) and none of `< > : " | ? *`.
//! - No component ends with `.` or a space.
//! - No component is a Windows reserved device name (with or without extension,
//!   case-insensitive): `CON PRN AUX NUL COM0-9 LPT0-9 CONIN$ CONOUT$ COM¹²³ LPT¹²³`.
//! - The first component is not `.vgames` (any case).
//! - Defense in depth: the NFKC form of each component must pass the same
//!   structural and character rules, so that no compatibility normalization
//!   (for example `‥` → `..`, `／` → `/`, fullwidth `＜`) can turn an accepted
//!   path into one that escapes the install root.
//!
//! Tree rules ([`validate_tree`]): paths are unique under Unicode **simple case
//! folding** (portable to NTFS and APFS), every directory prefix is spelled the
//! same way everywhere, and no file is also a directory.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use unicode_normalization::{UnicodeNormalization, is_nfc};

mod casefold;

pub use casefold::UNICODE_CASE_FOLDING_VERSION;

pub const MAX_PATH_BYTES: usize = 512;
pub const MAX_COMPONENT_BYTES: usize = 255;
pub const MAX_COMPONENTS: usize = 64;
/// First component reserved for launcher metadata inside an install.
pub const RESERVED_ROOT: &str = ".vgames";

/// Why a path was refused. Every variant carries the offending path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("{0:?} is not in Unicode NFC")]
    NotNfc(String),
    #[error("{0:?} is empty, absolute, uses '\\', or has an empty, '.' or '..' component")]
    BadStructure(String),
    #[error(
        "{0:?} is longer than {MAX_PATH_BYTES} bytes, has a component over {MAX_COMPONENT_BYTES} bytes, or more than {MAX_COMPONENTS} components"
    )]
    TooLong(String),
    #[error("{0:?} contains a control character or one of < > : \" | ? *")]
    ForbiddenCharacter(String),
    #[error("{0:?} has a component ending with '.' or a space")]
    TrailingDotOrSpace(String),
    #[error("{0:?} uses a name reserved by Windows")]
    ReservedName(String),
    #[error("{0:?} is inside the reserved .vgames folder")]
    ReservedRoot(String),
    #[error("{0:?} becomes unsafe under Unicode compatibility normalization")]
    UnsafeCompatibilityForm(String),
    #[error("{0:?} and {1:?} differ only by letter case")]
    CaseCollision(String, String),
    #[error("{0:?} is both a file and a folder")]
    FileIsDirectory(String),
    #[error("{0:?} is listed twice")]
    Duplicate(String),
}

impl PathError {
    /// The (first) path the error is about.
    pub fn path(&self) -> &str {
        match self {
            PathError::NotNfc(p)
            | PathError::BadStructure(p)
            | PathError::TooLong(p)
            | PathError::ForbiddenCharacter(p)
            | PathError::TrailingDotOrSpace(p)
            | PathError::ReservedName(p)
            | PathError::ReservedRoot(p)
            | PathError::UnsafeCompatibilityForm(p)
            | PathError::CaseCollision(p, _)
            | PathError::FileIsDirectory(p)
            | PathError::Duplicate(p) => p,
        }
    }
}

/// Device names, compared after simple case folding of the stem.
const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "conin$", "conout$", "com0", "com1", "com2", "com3", "com4",
    "com5", "com6", "com7", "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6",
    "lpt7", "lpt8", "lpt9", "com¹", "com²", "com³", "lpt¹", "lpt²", "lpt³",
];

fn is_forbidden_char(c: char) -> bool {
    c <= '\u{1f}' || c == '\u{7f}' || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')
}

/// Unicode simple case folding of one character (CaseFolding.txt, C + S).
pub fn fold_char(c: char) -> char {
    let cp = u32::from(c);
    match casefold::SIMPLE_CASE_FOLDING.binary_search_by_key(&cp, |&(from, _)| from) {
        Ok(i) => casefold::SIMPLE_CASE_FOLDING
            .get(i)
            .and_then(|&(_, to)| char::from_u32(to))
            .unwrap_or(c),
        Err(_) => c,
    }
}

/// Unicode simple case folding of a string (length in chars is preserved).
pub fn fold(s: &str) -> String {
    s.chars().map(fold_char).collect()
}

/// The component rules that must hold both for the component itself and for its NFKC form.
fn check_component_chars(component: &str) -> Result<(), ComponentFault> {
    if component.is_empty() || component == "." || component == ".." {
        return Err(ComponentFault::Structure);
    }
    if component.contains(['/', '\\']) {
        return Err(ComponentFault::Structure);
    }
    if component.chars().any(is_forbidden_char) {
        return Err(ComponentFault::Char);
    }
    if component.ends_with('.') || component.ends_with(' ') {
        return Err(ComponentFault::Trailing);
    }
    if is_reserved_device(component) {
        return Err(ComponentFault::Reserved);
    }
    Ok(())
}

enum ComponentFault {
    Structure,
    Char,
    Trailing,
    Reserved,
}

fn is_reserved_device(component: &str) -> bool {
    // Windows ignores everything from the first dot, and trailing spaces before it.
    let stem = component.split('.').next().unwrap_or(component);
    let stem = fold(stem.trim_end_matches(' '));
    WINDOWS_RESERVED.contains(&stem.as_str())
}

/// Checks one path against every per-path rule.
pub fn validate_path(path: &str) -> Result<(), PathError> {
    let owned = || path.to_owned();
    if path.len() > MAX_PATH_BYTES {
        return Err(PathError::TooLong(owned()));
    }
    if !is_nfc(path) {
        return Err(PathError::NotNfc(owned()));
    }
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return Err(PathError::BadStructure(owned()));
    }
    let mut count = 0usize;
    for (i, component) in path.split('/').enumerate() {
        count += 1;
        if count > MAX_COMPONENTS {
            return Err(PathError::TooLong(owned()));
        }
        if component.len() > MAX_COMPONENT_BYTES {
            return Err(PathError::TooLong(owned()));
        }
        check_component_chars(component).map_err(|f| match f {
            ComponentFault::Structure => PathError::BadStructure(owned()),
            ComponentFault::Char => PathError::ForbiddenCharacter(owned()),
            ComponentFault::Trailing => PathError::TrailingDotOrSpace(owned()),
            ComponentFault::Reserved => PathError::ReservedName(owned()),
        })?;
        if i == 0 && fold(component) == RESERVED_ROOT {
            return Err(PathError::ReservedRoot(owned()));
        }
        let compat: String = component.nfkc().collect();
        if compat != component {
            let unsafe_form = check_component_chars(&compat).is_err()
                || (i == 0 && fold(&compat) == RESERVED_ROOT);
            if unsafe_form {
                return Err(PathError::UnsafeCompatibilityForm(owned()));
            }
        }
    }
    Ok(())
}

/// Checks a whole tree: every file and directory path, plus the set rules
/// (unique under case folding, consistent directory spelling, no file that is
/// also a directory). Directories may be listed explicitly (empty folders) or
/// implied by file paths.
pub fn validate_tree<'a>(
    files: impl IntoIterator<Item = &'a str>,
    directories: impl IntoIterator<Item = &'a str>,
) -> Result<(), PathError> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Kind {
        File,
        ListedDir,
        ImpliedDir,
    }
    // folded path → (exact spelling, kind)
    let mut seen: HashMap<String, (&'a str, Kind)> = HashMap::new();

    let mut insert = |path: &'a str, kind: Kind| -> Result<(), PathError> {
        match seen.entry(fold(path)) {
            Entry::Vacant(v) => {
                v.insert((path, kind));
                Ok(())
            }
            Entry::Occupied(mut o) => {
                let (other, other_kind) = *o.get();
                if other != path {
                    return Err(PathError::CaseCollision(other.to_owned(), path.to_owned()));
                }
                match (other_kind, kind) {
                    (Kind::ImpliedDir, Kind::ImpliedDir) => Ok(()),
                    (Kind::ImpliedDir, Kind::ListedDir) => {
                        o.insert((path, Kind::ListedDir));
                        Ok(())
                    }
                    (Kind::ListedDir, Kind::ImpliedDir) => Ok(()),
                    (Kind::File, Kind::File) | (Kind::ListedDir, Kind::ListedDir) => {
                        Err(PathError::Duplicate(path.to_owned()))
                    }
                    (Kind::File, Kind::ListedDir) | (Kind::ListedDir, Kind::File) => {
                        Err(PathError::Duplicate(path.to_owned()))
                    }
                    (Kind::File, Kind::ImpliedDir) | (Kind::ImpliedDir, Kind::File) => {
                        Err(PathError::FileIsDirectory(path.to_owned()))
                    }
                }
            }
        }
    };

    let mut add = |path: &'a str, kind: Kind| -> Result<(), PathError> {
        validate_path(path)?;
        insert(path, kind)?;
        let mut prefix = path;
        while let Some((parent, _)) = prefix.rsplit_once('/') {
            insert(parent, Kind::ImpliedDir)?;
            prefix = parent;
        }
        Ok(())
    };
    for f in files {
        add(f, Kind::File)?;
    }
    for d in directories {
        add(d, Kind::ListedDir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
