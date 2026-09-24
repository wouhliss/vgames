//! Changelog fragments (`.changes/*.md`, 08-release §3). Every PR adds one,
//! written by the agent that made the change. `audience: user` text is shown
//! verbatim to players, so it must read like a sentence for players, not like a
//! commit message: [`lint_user_text`] enforces 08 §3.2 and cannot be overridden.

use std::fmt;
use std::sync::LazyLock;

use regex::Regex;

pub const MIN_USER_CHARS: usize = 10;
pub const MAX_USER_CHARS: usize = 240;
pub const MAX_INTERNAL_CHARS: usize = 1000;
pub const MAX_SLUG_CHARS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Audience {
    User,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Component {
    Launcher,
    Admin,
    Server,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ChangeType {
    Added,
    Changed,
    Fixed,
    Removed,
    Security,
}

impl Audience {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "internal" => Some(Self::Internal),
            _ => None,
        }
    }
}

impl Component {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "launcher" => Some(Self::Launcher),
            "admin" => Some(Self::Admin),
            "server" => Some(Self::Server),
            _ => None,
        }
    }
}

impl ChangeType {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "added" => Some(Self::Added),
            "changed" => Some(Self::Changed),
            "fixed" => Some(Self::Fixed),
            "removed" => Some(Self::Removed),
            "security" => Some(Self::Security),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Changed => "changed",
            Self::Fixed => "fixed",
            Self::Removed => "removed",
            Self::Security => "security",
        }
    }
}

impl fmt::Display for ChangeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A parsed, valid fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub slug: String,
    pub audience: Audience,
    pub component: Component,
    pub kind: ChangeType,
    pub text: String,
}

/// Technical words players should never read (08 §3.2), matched
/// case-insensitively on word boundaries, with common inflections
/// ("Refactored", "Bumped", "tests").
const DENYLIST: &[&str] = &[
    "refactor",
    "dependency",
    "dependencies",
    "bump",
    "upgrade to",
    "crate",
    "npm",
    "pnpm",
    "cargo",
    "CI",
    "pipeline",
    "lint",
    "clippy",
    "test",
    "tests",
    "typescript",
    "rust",
    "tauri",
    "react",
    "sqlx",
    "API",
    "endpoint",
    "schema",
    "migration",
    "internal",
    "codebase",
    "PR",
    "commit",
];

/// Words that give away how to attack something; security notes describe impact only.
const EXPLOIT_WORDS: &[&str] = &[
    "exploit",
    "payload",
    "injection",
    "inject",
    "overflow",
    "RCE",
    "XSS",
    "CSRF",
    "SSRF",
    "bypass",
    "PoC",
    "proof of concept",
    "shellcode",
    "CVE",
    "GHSA",
];

fn word_regex(words: &[&str]) -> Regex {
    let alternation = words
        .iter()
        .map(|w| regex::escape(w).replace(' ', r"\s+"))
        .collect::<Vec<_>>()
        .join("|");
    #[allow(clippy::expect_used)] // A constant pattern: covered by tests.
    Regex::new(&format!(
        r"(?i)\b(?:{alternation})(?:s|es|ed|d|ing|ings|er|ers)?\b"
    ))
    .expect("valid denylist regex")
}

#[allow(clippy::expect_used)] // Constant patterns: covered by tests.
static PATTERNS: LazyLock<[(Regex, &'static str); 5]> = LazyLock::new(|| {
    [
        (Regex::new("`").expect("re"), "backticks (code)"),
        (Regex::new(r"\w+/\w+").expect("re"), "a file path"),
        (
            Regex::new(r"(?i)\.(rs|ts|tsx|json|yml|yaml|toml)\b").expect("re"),
            "a file name",
        ),
        (Regex::new(r"#\d+").expect("re"), "an issue or PR number"),
        (Regex::new(r"(?i)\b[0-9a-f]{7,}\b").expect("re"), "a hash"),
    ]
});
static DENY: LazyLock<Regex> = LazyLock::new(|| word_regex(DENYLIST));
static EXPLOIT: LazyLock<Regex> = LazyLock::new(|| word_regex(EXPLOIT_WORDS));
#[allow(clippy::expect_used)]
static SECURITY_FORM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^fixed\s+(?:an?|the|some)\s+\w+(?:\s+\w+)*?\s+(?:that|which)\s+could\b")
        .expect("re")
});

/// Every rule for player-facing text. Returns human-readable problems (empty = valid).
pub fn lint_user_text(text: &str, kind: ChangeType) -> Vec<String> {
    let mut errors = Vec::new();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = text.chars().count();
    if !(MIN_USER_CHARS..=MAX_USER_CHARS).contains(&n) {
        errors.push(format!(
            "must be {MIN_USER_CHARS}–{MAX_USER_CHARS} characters (is {n})"
        ));
    }
    if !text.chars().next().is_some_and(char::is_uppercase) {
        errors.push("must start with a capital letter".into());
    }
    if !text.ends_with('.') {
        errors.push("must end with a period".into());
    }
    let sentences = text
        .char_indices()
        .filter(|(i, c)| {
            matches!(c, '.' | '!' | '?')
                && text[i + c.len_utf8()..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
        })
        .count();
    if sentences > 2 {
        errors.push(format!("must be one or two sentences (found {sentences})"));
    }
    let mut words: Vec<&str> = DENY.find_iter(&text).map(|m| m.as_str()).collect();
    words.dedup();
    if !words.is_empty() {
        errors.push(format!(
            "uses technical wording players should not see: {}",
            words.join(", ")
        ));
    }
    for (re, what) in PATTERNS.iter() {
        let hit = re.find_iter(&text).any(|m| {
            // A hash needs at least one digit ("defaced" is a word, "3fa9c1e" is not).
            *what != "a hash" || m.as_str().bytes().any(|b| b.is_ascii_digit())
        });
        if hit {
            errors.push(format!("contains {what}"));
        }
    }
    if kind == ChangeType::Security {
        if !SECURITY_FORM.is_match(&text) {
            errors.push(
                "security notes describe the impact on players, e.g. \"Fixed an issue that could …\""
                    .into(),
            );
        }
        let mut hits: Vec<&str> = EXPLOIT.find_iter(&text).map(|m| m.as_str()).collect();
        hits.dedup();
        if !hits.is_empty() {
            errors.push(format!(
                "security notes must not give exploit details: {}",
                hits.join(", ")
            ));
        }
    }
    errors
}

fn valid_slug(file_name: &str) -> Option<&str> {
    let slug = file_name.strip_suffix(".md")?;
    let ok = (1..=MAX_SLUG_CHARS).contains(&slug.len())
        && slug.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        });
    ok.then_some(slug)
}

/// Parses and lints one fragment file. `Err` lists every problem found.
pub fn parse_fragment(file_name: &str, content: &str) -> Result<Fragment, Vec<String>> {
    let mut errors = Vec::new();
    let slug = valid_slug(file_name);
    if slug.is_none() {
        errors.push(format!(
            "file name must be <short-kebab-slug>.md (lowercase letters, digits, dashes; ≤ {MAX_SLUG_CHARS})"
        ));
    }
    let content = content.replace("\r\n", "\n");
    let Some(rest) = content.strip_prefix("---\n") else {
        errors.push("must start with a `---` frontmatter block".into());
        return Err(errors);
    };
    let Some((front, body)) = rest
        .split_once("\n---\n")
        .or_else(|| rest.strip_suffix("\n---").map(|f| (f, "")))
    else {
        errors.push("frontmatter block is not closed with `---`".into());
        return Err(errors);
    };
    let (mut audience, mut component, mut kind) = (None, None, None);
    for line in front.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            errors.push(format!("frontmatter line {line:?} is not `key: value`"));
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        let dup = |seen: bool| {
            if seen {
                Some(format!("`{key}` is set twice"))
            } else {
                None
            }
        };
        match key {
            "audience" => {
                errors.extend(dup(audience.is_some()));
                audience = Audience::parse(value);
                if audience.is_none() {
                    errors.push(format!("audience must be user or internal, not {value:?}"));
                }
            }
            "component" => {
                errors.extend(dup(component.is_some()));
                component = Component::parse(value);
                if component.is_none() {
                    errors.push(format!(
                        "component must be launcher, admin or server, not {value:?}"
                    ));
                }
            }
            "type" => {
                errors.extend(dup(kind.is_some()));
                kind = ChangeType::parse(value);
                if kind.is_none() {
                    errors.push(format!(
                        "type must be added, changed, fixed, removed or security, not {value:?}"
                    ));
                }
            }
            other => errors.push(format!("unknown frontmatter key {other:?}")),
        }
    }
    let text = body.trim().to_owned();
    if text.is_empty() {
        errors.push("the fragment has no text".into());
    }
    match (audience, component, kind) {
        (Some(audience), Some(component), Some(kind)) if errors.is_empty() => {
            if audience == Audience::User {
                errors.extend(lint_user_text(&text, kind));
            } else if text.chars().count() > MAX_INTERNAL_CHARS {
                errors.push(format!(
                    "internal text must be at most {MAX_INTERNAL_CHARS} characters"
                ));
            }
            if errors.is_empty() {
                return Ok(Fragment {
                    slug: slug.unwrap_or_default().to_owned(),
                    audience,
                    component,
                    kind,
                    text,
                });
            }
        }
        (a, c, k) => {
            for (missing, name) in [
                (a.is_none(), "audience"),
                (c.is_none(), "component"),
                (k.is_none(), "type"),
            ] {
                if missing && !errors.iter().any(|e| e.starts_with(name)) {
                    errors.push(format!("frontmatter needs `{name}`"));
                }
            }
        }
    }
    Err(errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Vec<String> {
        lint_user_text(text, ChangeType::Added)
    }

    #[test]
    fn acceptance_examples() {
        assert!(!user("Bumped tauri to 2.12").is_empty());
        assert!(!user("Bumped tauri to 2.12.").is_empty());
        assert!(!user("Refactored the download engine").is_empty());
        assert!(!user("Refactored the download engine.").is_empty());
        assert_eq!(
            user("Downloads now resume after your computer restarts."),
            Vec::<String>::new()
        );
    }

    #[test]
    fn every_denylist_word_is_caught_with_inflections() {
        for word in DENYLIST {
            let t = format!("We changed the {word} for you.");
            assert!(user(&t).iter().any(|e| e.contains("technical")), "{word}");
        }
        for t in [
            "Bumped the version for you.",
            "The tests were improved for you.",
            "Upgrade to the new menu now.",
            "Two PRs made the menu faster.",
            "Refactoring made the menu faster.",
            "The api is faster now.",
        ] {
            assert!(user(t).iter().any(|e| e.contains("technical")), "{t}");
        }
        // Word boundaries: these are ordinary words.
        for t in [
            "Your library loads faster when you have many games.",
            "Trusted servers now show a green check mark.",
            "Pressing the guide button opens the overlay.",
            "Your pictures are cropped less in the store.",
            "Latest releases appear first in your library.",
        ] {
            assert_eq!(user(t), Vec::<String>::new(), "{t}");
        }
    }

    #[test]
    fn patterns() {
        for (t, what) in [
            ("You can now use `vgames` anywhere.", "backticks"),
            ("Saves in Game/Saved sync again.", "file path"),
            ("The settings.json file loads faster.", "file name"),
            ("Fixed the crash from #123 on start.", "issue"),
            ("Fixed a crash introduced in 3fa9c1e today.", "hash"),
        ] {
            assert!(
                user(t).iter().any(|e| e.contains(what)),
                "{t}: {:?}",
                user(t)
            );
        }
        assert_eq!(
            user("Faded covers look defaced no more."),
            Vec::<String>::new()
        );
    }

    #[test]
    fn sentence_form() {
        assert!(
            user("you can now pin favorites.")
                .iter()
                .any(|e| e.contains("capital"))
        );
        assert!(
            user("You can now pin favorites")
                .iter()
                .any(|e| e.contains("period"))
        );
        assert!(user("Short.").iter().any(|e| e.contains("characters")));
        assert!(
            user(&format!("{}.", "A".repeat(240)))
                .iter()
                .any(|e| e.contains("characters"))
        );
        assert!(
            user("One thing. Two things. Three things.")
                .iter()
                .any(|e| e.contains("sentences"))
        );
        assert_eq!(
            user("You can pin favorites. They stay on top."),
            Vec::<String>::new()
        );
    }

    #[test]
    fn security_wording() {
        let sec = |t: &str| lint_user_text(t, ChangeType::Security);
        assert_eq!(
            sec("Fixed an issue that could let a malicious server show a fake install prompt."),
            Vec::<String>::new()
        );
        assert!(!sec("Improved security.").is_empty());
        assert!(
            !sec("Fixed an issue that could allow script injection in descriptions.").is_empty()
        );
        assert!(
            !sec("Fixed an issue that could let a server bypass the fingerprint check.").is_empty()
        );
    }

    const OK: &str = "---\naudience: user            # user | internal\ncomponent: launcher\ntype: added\n---\nYou can now pin favorite packages to the top of your library.\n";

    #[test]
    fn fragment_parsing() {
        let f = parse_fragment("pin-favorites.md", OK).unwrap();
        assert_eq!(f.audience, Audience::User);
        assert_eq!(f.component, Component::Launcher);
        assert_eq!(f.kind, ChangeType::Added);
        assert_eq!(f.slug, "pin-favorites");
        let internal = "---\naudience: internal\ncomponent: server\ntype: changed\n---\nRefactored the `jobs` crate.\n";
        parse_fragment("jobs-refactor.md", internal).unwrap();
    }

    #[test]
    fn fragment_errors() {
        for name in [
            "Pin.md",
            "pin_favorites.md",
            "pin-.md",
            "pin.txt",
            "-pin.md",
        ] {
            assert!(parse_fragment(name, OK).is_err(), "{name}");
        }
        let bad = |c: &str| parse_fragment("x.md", c).unwrap_err();
        assert!(
            bad("no frontmatter")
                .iter()
                .any(|e| e.contains("frontmatter"))
        );
        assert!(
            bad("---\naudience: user\n")
                .iter()
                .any(|e| e.contains("closed"))
        );
        assert!(
            bad(&OK.replace("audience: user", "audience: players"))
                .iter()
                .any(|e| e.contains("audience"))
        );
        assert!(
            bad(&OK.replace("type: added", "type: feature"))
                .iter()
                .any(|e| e.contains("type"))
        );
        assert!(
            bad(&OK.replace("component: launcher", "component: desktop"))
                .iter()
                .any(|e| e.contains("component"))
        );
        assert!(
            bad(&OK.replace("type: added\n", ""))
                .iter()
                .any(|e| e.contains("needs `type`"))
        );
        assert!(
            bad(&OK.replace("type: added", "type: added\ntype: fixed"))
                .iter()
                .any(|e| e.contains("twice"))
        );
        assert!(
            bad(&OK.replace("type: added", "type: added\nscope: ui"))
                .iter()
                .any(|e| e.contains("unknown"))
        );
        assert!(
            bad("---\naudience: user\ncomponent: launcher\ntype: added\n---\n\n")
                .iter()
                .any(|e| e.contains("no text"))
        );
        assert!(
            bad(&OK.replace("You can now", "Refactored so you can now"))
                .iter()
                .any(|e| e.contains("technical"))
        );
    }

    #[test]
    fn existing_fragments_in_the_repo_are_valid() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.changes");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            if name == "README.md" {
                continue;
            }
            let content = std::fs::read_to_string(&path).unwrap();
            if let Err(e) = parse_fragment(&name, &content) {
                panic!("{name}: {e:?}");
            }
        }
    }
}
