//! Manifest path rules (02-package-format §3), used by the packer to refuse an
//! invalid tree before anything is uploaded.
//!
//! **Interim implementation** until `vgames_core::paths` (A5-T02) lands. The
//! server and the launcher validate with core; this copy only gives the
//! publisher early feedback, so being stricter is harmless while being looser
//! is not. Switch to core and delete this file once it is available.

use std::collections::HashSet;

use unicode_normalization::is_nfc;

pub const MAX_PATH_BYTES: usize = 512;
pub const MAX_COMPONENT_BYTES: usize = 255;
pub const MAX_COMPONENTS: usize = 64;
pub const RESERVED_ROOT: &str = ".vgames";

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
    #[error("{0:?} and {1:?} differ only by letter case")]
    CaseCollision(String, String),
    #[error("{0:?} is both a file and a folder")]
    FileIsDirectory(String),
    #[error("{0:?} is listed twice")]
    Duplicate(String),
}

const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "conin$", "conout$", "com0", "com1", "com2", "com3", "com4",
    "com5", "com6", "com7", "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6",
    "lpt7", "lpt8", "lpt9", "com¹", "com²", "com³", "lpt¹", "lpt²", "lpt³",
];

/// Checks one path against every per-path rule.
pub fn validate_path(path: &str) -> Result<(), PathError> {
    let owned = || path.to_owned();
    if !is_nfc(path) {
        return Err(PathError::NotNfc(owned()));
    }
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return Err(PathError::BadStructure(owned()));
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(PathError::TooLong(owned()));
    }
    let components: Vec<&str> = path.split('/').collect();
    if components.len() > MAX_COMPONENTS {
        return Err(PathError::TooLong(owned()));
    }
    if components
        .first()
        .is_some_and(|c| c.eq_ignore_ascii_case(RESERVED_ROOT))
    {
        return Err(PathError::ReservedRoot(owned()));
    }
    for component in components {
        if component.is_empty() || component == "." || component == ".." {
            return Err(PathError::BadStructure(owned()));
        }
        if component.len() > MAX_COMPONENT_BYTES {
            return Err(PathError::TooLong(owned()));
        }
        if component.chars().any(|c| {
            c <= '\u{1f}' || c == '\u{7f}' || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')
        }) {
            return Err(PathError::ForbiddenCharacter(owned()));
        }
        if component.ends_with('.') || component.ends_with(' ') {
            return Err(PathError::TrailingDotOrSpace(owned()));
        }
        let stem = component
            .split('.')
            .next()
            .unwrap_or(component)
            .to_lowercase();
        if WINDOWS_RESERVED.contains(&stem.as_str()) {
            return Err(PathError::ReservedName(owned()));
        }
    }
    Ok(())
}

fn fold(path: &str) -> String {
    // Approximates Unicode simple case folding; core implements the exact table.
    path.chars().flat_map(char::to_lowercase).collect()
}

/// Checks a whole tree: every file and directory path, plus the set rules
/// (unique under case folding, no file doubling as a directory).
pub fn validate_tree<'a>(
    files: impl IntoIterator<Item = &'a str>,
    directories: impl IntoIterator<Item = &'a str>,
) -> Result<(), PathError> {
    let mut exact: HashSet<&str> = HashSet::new();
    let mut folded: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
    let mut file_paths: Vec<&str> = Vec::new();
    let mut all_dirs: HashSet<String> = HashSet::new();

    let mut add = |path: &'a str, is_file: bool| -> Result<(), PathError> {
        validate_path(path)?;
        if !exact.insert(path) {
            return Err(PathError::Duplicate(path.to_owned()));
        }
        if let Some(other) = folded.insert(fold(path), path) {
            return Err(PathError::CaseCollision(other.to_owned(), path.to_owned()));
        }
        if is_file {
            file_paths.push(path);
        }
        let mut prefix = path;
        while let Some((parent, _)) = prefix.rsplit_once('/') {
            all_dirs.insert(fold(parent));
            prefix = parent;
        }
        Ok(())
    };
    for f in files {
        add(f, true)?;
    }
    for d in directories {
        add(d, false)?;
    }
    for f in file_paths {
        if all_dirs.contains(&fold(f)) {
            return Err(PathError::FileIsDirectory(f.to_owned()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_paths() {
        for p in [
            "Game/bin/game.exe",
            "a",
            "日本語/ファイル.txt",
            "Saved/.hidden",
            "con_file",
            "console.log",
        ] {
            validate_path(p).unwrap();
        }
    }

    #[test]
    fn rejects_each_rule() {
        let decomposed = "e\u{301}.txt";
        assert!(matches!(
            validate_path(decomposed),
            Err(PathError::NotNfc(_))
        ));
        for p in ["", "/abs", "a\\b", "a//b", "./a", "a/../b", "a/."] {
            assert!(
                matches!(validate_path(p), Err(PathError::BadStructure(_))),
                "{p}"
            );
        }
        assert!(matches!(
            validate_path(&"a".repeat(256)),
            Err(PathError::TooLong(_))
        ));
        validate_path(&vec!["a"; 64].join("/")).unwrap();
        assert!(matches!(
            validate_path(&vec!["a"; 65].join("/")),
            Err(PathError::TooLong(_))
        ));
        assert!(matches!(
            validate_path(&format!("{}/{}", "a".repeat(255), "b".repeat(257))),
            Err(PathError::TooLong(_))
        ));
        for p in [
            "a<b", "a:b", "a|b", "a?b", "a*b", "a\"b", "a\u{1}b", "a\u{7f}b", "a>b",
        ] {
            assert!(
                matches!(validate_path(p), Err(PathError::ForbiddenCharacter(_))),
                "{p}"
            );
        }
        for p in ["a.", "a ", "dir./x"] {
            assert!(
                matches!(validate_path(p), Err(PathError::TrailingDotOrSpace(_))),
                "{p}"
            );
        }
        for p in [
            "CON",
            "con.txt",
            "x/Nul.tar.gz",
            "COM1",
            "lpt9.log",
            "CONIN$",
            "com¹",
            "LPT³.x",
        ] {
            assert!(
                matches!(validate_path(p), Err(PathError::ReservedName(_))),
                "{p}"
            );
        }
        for p in [".vgames/x", ".VGames"] {
            assert!(
                matches!(validate_path(p), Err(PathError::ReservedRoot(_))),
                "{p}"
            );
        }
    }

    #[test]
    fn tree_rules() {
        validate_tree(["a/b", "a/c"], ["d"]).unwrap();
        assert!(matches!(
            validate_tree(["a/B", "A/b"], []),
            Err(PathError::CaseCollision(..))
        ));
        assert!(matches!(
            validate_tree(["a", "a/b"], []),
            Err(PathError::FileIsDirectory(_))
        ));
        assert!(matches!(
            validate_tree(["a"], ["a/x"]),
            Err(PathError::FileIsDirectory(_))
        ));
        assert!(matches!(
            validate_tree(["a", "a"], []),
            Err(PathError::Duplicate(_))
        ));
        assert!(matches!(
            validate_tree(["a"], ["a"]),
            Err(PathError::Duplicate(_))
        ));
    }
}
