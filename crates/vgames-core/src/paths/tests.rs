use proptest::prelude::*;
use unicode_normalization::UnicodeNormalization;

use super::*;

fn err(p: &str) -> PathError {
    validate_path(p).unwrap_err()
}

#[test]
fn accepts_normal_paths() {
    for p in [
        "Game/bin/game.exe",
        "a",
        "日本語/ファイル.txt",
        "Saved/.hidden",
        "con_file",
        "console.log",
        "COM10",
        "LPT",
        ".vgamesx/a",
        "a/.vgames",
        "é.txt",
        "½.txt",
    ] {
        validate_path(p).unwrap_or_else(|e| panic!("{p}: {e}"));
    }
    validate_path(&["a"; 64].join("/")).unwrap();
    validate_path(&"a".repeat(255)).unwrap();
    validate_path(&format!("{}/{}", "a".repeat(255), "b".repeat(255))).unwrap();
}

#[test]
fn rule_nfc() {
    assert!(matches!(err("e\u{301}.txt"), PathError::NotNfc(_)));
}

#[test]
fn rule_structure() {
    for p in [
        "", "/abs", "a\\b", "a//b", "./a", "a/../b", "a/.", "a/", "..", ".",
    ] {
        assert!(matches!(err(p), PathError::BadStructure(_)), "{p:?}");
    }
}

#[test]
fn rule_lengths() {
    assert!(matches!(err(&"a".repeat(256)), PathError::TooLong(_)));
    assert!(matches!(err(&["a"; 65].join("/")), PathError::TooLong(_)));
    let long = format!("{}/{}/{}", "a".repeat(255), "b".repeat(255), "c");
    assert_eq!(long.len(), 513);
    assert!(matches!(err(&long), PathError::TooLong(_)));
    // Multi-byte characters count in bytes: 86 × 3 bytes = 258.
    assert!(matches!(err(&"語".repeat(86)), PathError::TooLong(_)));
}

#[test]
fn rule_forbidden_characters() {
    for p in [
        "a<b", "a:b", "a|b", "a?b", "a*b", "a\"b", "a\u{1}b", "a\u{7f}b", "a>b", "a\u{0}b",
        "a\u{1f}", "a\nb",
    ] {
        assert!(matches!(err(p), PathError::ForbiddenCharacter(_)), "{p:?}");
    }
}

#[test]
fn rule_trailing_dot_or_space() {
    for p in ["a.", "a ", "dir./x", "dir /x", "x/a.."] {
        assert!(matches!(err(p), PathError::TrailingDotOrSpace(_)), "{p:?}");
    }
}

#[test]
fn rule_reserved_names() {
    for p in [
        "CON",
        "con.txt",
        "x/Nul.tar.gz",
        "COM1",
        "COM0",
        "lpt9.log",
        "LPT0",
        "CONIN$",
        "conout$.x",
        "com¹",
        "LPT³.x",
        "Com² ",
        "AUX .txt",
        "prn",
    ] {
        let e = err(p);
        assert!(
            matches!(
                e,
                PathError::ReservedName(_) | PathError::TrailingDotOrSpace(_)
            ),
            "{p:?}: {e:?}"
        );
    }
    for p in ["CON", "con.txt", "com¹", "AUX .txt", "Nul.tar.gz"] {
        assert!(matches!(err(p), PathError::ReservedName(_)), "{p:?}");
    }
}

#[test]
fn rule_reserved_root() {
    for p in [".vgames", ".vgames/x", ".VGames/install.json"] {
        assert!(matches!(err(p), PathError::ReservedRoot(_)), "{p:?}");
    }
}

#[test]
fn rule_compatibility_forms() {
    for p in [
        "\u{2025}",                 // two dot leader → ".."
        "a/\u{2025}/b",             // …inside a path
        "\u{ff0e}\u{ff0e}",         // fullwidth full stops → ".."
        "a\u{ff0f}b",               // fullwidth solidus → "/"
        "a\u{ff3c}b",               // fullwidth reverse solidus → "\"
        "a\u{ff1c}b",               // fullwidth less-than → "<"
        "a\u{a0}",                  // trailing NBSP → trailing space
        "\u{ff43}\u{ff4f}\u{ff4e}", // fullwidth "con"
        "\u{ff0e}vgames/x",         // fullwidth dot + "vgames"
        "a\u{2024}",                // one dot leader → trailing "."
    ] {
        assert!(
            matches!(err(p), PathError::UnsafeCompatibilityForm(_)),
            "{p:?}: {:?}",
            validate_path(p)
        );
    }
}

#[test]
fn simple_case_folding_is_exact() {
    assert_eq!(fold("ABC"), "abc");
    assert_eq!(fold("ſ"), "s"); // U+017F LATIN SMALL LETTER LONG S (C)
    assert_eq!(fold("\u{212a}"), "k"); // KELVIN SIGN (C)
    assert_eq!(fold("ς"), "σ"); // final sigma folds (C)
    assert_eq!(fold("ẞ"), "ß"); // capital sharp s: simple folding (S), not "ss"
    assert_eq!(fold("İ"), "İ"); // only T/F mappings: unchanged by simple folding
    assert_eq!(fold("Ꭰ"), "Ꭰ"); // Cherokee capitals are the folded form
    assert_eq!(fold("ꭰ"), "Ꭰ"); // Cherokee small letters fold to capitals
    assert_eq!(UNICODE_CASE_FOLDING_VERSION, "18.0.0");
    // The table is sorted (binary search relies on it).
    assert!(
        casefold::SIMPLE_CASE_FOLDING
            .windows(2)
            .all(|w| w[0].0 < w[1].0)
    );
}

#[test]
fn tree_rules() {
    validate_tree(["a/b", "a/c"], ["d", "a"]).unwrap();
    validate_tree([], []).unwrap();
    // Case-insensitive collisions, including exact Unicode folding.
    for (a, b) in [
        ("a/B", "A/b"),
        ("x/ſ.txt", "x/s.txt"),
        ("ǅ", "ǆ"), // titlecase digraph folds to lowercase
        ("σ", "ς"),
    ] {
        assert!(
            matches!(validate_tree([a, b], []), Err(PathError::CaseCollision(..))),
            "{a} {b}"
        );
    }
    // Directory prefixes must be spelled the same way everywhere.
    assert!(matches!(
        validate_tree(["Game/a", "game/b"], []),
        Err(PathError::CaseCollision(..))
    ));
    assert!(matches!(
        validate_tree(["Game/a"], ["game"]),
        Err(PathError::CaseCollision(..))
    ));
    // A file that is also a folder.
    assert!(matches!(
        validate_tree(["a", "a/b"], []),
        Err(PathError::FileIsDirectory(_))
    ));
    assert!(matches!(
        validate_tree(["a/b", "a"], []),
        Err(PathError::FileIsDirectory(_))
    ));
    assert!(matches!(
        validate_tree(["a"], ["a/x"]),
        Err(PathError::FileIsDirectory(_))
    ));
    // Listed twice.
    assert!(matches!(
        validate_tree(["a", "a"], []),
        Err(PathError::Duplicate(_))
    ));
    assert!(matches!(
        validate_tree(["a"], ["a"]),
        Err(PathError::Duplicate(_))
    ));
    assert!(matches!(
        validate_tree([], ["d", "d"]),
        Err(PathError::Duplicate(_))
    ));
    // Per-path rules apply to directories too.
    assert!(matches!(
        validate_tree([], ["a/../b"]),
        Err(PathError::BadStructure(_))
    ));
}

/// Characters chosen to stress every rule and every normalization.
fn tricky_char() -> impl Strategy<Value = char> {
    prop::sample::select(vec![
        'a', 'B', 'c', 'o', 'n', 'N', 'C', 'O', '1', '.', '/', '\\', ' ', ':', '\u{0}', 'e',
        '\u{301}', '\u{2024}', '\u{2025}', '\u{2026}', '\u{ff0e}', '\u{ff0f}', '\u{ff3c}',
        '\u{a0}', '\u{2000}', 'ſ', 'K', '\u{212a}', '¹', '²', 'İ', 'ı', '\u{fe52}', '\u{2e}',
        '\u{2215}', '\u{29f8}', '\u{ff1a}', 'v', 'g', 'm', 's',
    ])
}

fn normalizations(p: &str) -> Vec<String> {
    vec![
        p.to_owned(),
        p.nfc().collect(),
        p.nfd().collect(),
        p.nfkc().collect(),
        p.nfkd().collect(),
        p.to_lowercase(),
        p.to_uppercase(),
        fold(p),
        p.to_lowercase().nfkc().collect(),
        p.to_uppercase().nfkc().collect(),
    ]
}

/// Lexically resolves `p` under a root the way Windows or POSIX would, after
/// Win32's trailing dot/space stripping. `None` means it escapes or aliases the root.
fn stays_under_root(p: &str) -> bool {
    if p.starts_with(['/', '\\']) || p.get(1..2) == Some(":") {
        return false;
    }
    let mut depth = 0i32;
    for comp in p.split(['/', '\\']) {
        let win = comp.trim_end_matches(['.', ' ']);
        match (comp, win) {
            ("", _) | (".", _) | ("..", _) | (_, "") => return false,
            _ => depth += 1,
        }
        if comp.contains(':') {
            return false; // drive-relative or alternate data stream
        }
    }
    depth > 0
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 20_000, ..ProptestConfig::default() })]

    /// Never accepts a path that escapes (or aliases) the root after any normalization.
    #[test]
    fn accepted_paths_never_escape(chars in prop::collection::vec(tricky_char(), 0..24)) {
        let p: String = chars.into_iter().collect();
        if validate_path(&p).is_ok() {
            for n in normalizations(&p) {
                prop_assert!(stays_under_root(&n), "{:?} → {:?}", p, n);
                let first = n.split(['/', '\\']).next().unwrap_or("");
                prop_assert_ne!(fold(first.trim_end_matches(['.', ' '])), RESERVED_ROOT, "{:?}", p);
            }
        }
    }

    /// Validation never panics on arbitrary input.
    #[test]
    fn never_panics(p in any::<String>()) {
        let _ = validate_path(&p);
    }

    /// Well-formed ASCII paths built from safe components are accepted.
    #[test]
    fn safe_paths_accepted(comps in prop::collection::vec("[a-z0-9_][a-z0-9_.-]{0,10}[a-z0-9_]", 1..8)) {
        let p = comps.join("/");
        let reserved = comps.iter().any(|c| is_reserved_device(c));
        prop_assert_eq!(validate_path(&p).is_ok(), !reserved, "{}", p);
    }

    /// Two different folded-equal spellings are always refused as a tree.
    #[test]
    fn case_variants_collide(name in "x[a-z]{1,12}") {
        let upper = name.to_uppercase();
        if upper != name {
            prop_assert!(
                matches!(validate_tree([name.as_str(), upper.as_str()], []), Err(PathError::CaseCollision(..))),
                "unexpected result for {:?}", name
            );
        }
    }
}
