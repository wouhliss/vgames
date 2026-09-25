//! `cargo xtask codeowners check` (A5-T10): every tracked file has a code owner,
//! every rule names one, and the security-critical paths are owned by the
//! security section of `.github/CODEOWNERS` (so a later rule cannot silently
//! take them over: in CODEOWNERS the last matching rule wins).

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use globset::{GlobBuilder, GlobMatcher};

/// Marks the start of the security-critical section in `.github/CODEOWNERS`.
pub const SECURITY_MARKER: &str = "# ---- SECURITY-CRITICAL";

/// Paths whose owner must be the security owner (01-security invariants).
/// Files do not need to exist yet: the rules are checked for these paths.
pub const SECURITY_CRITICAL: &[&str] = &[
    "crates/vgames-core/src/lib.rs",
    "crates/vgames-core/src/sign.rs",
    "crates/vgames-transfer/src/lib.rs",
    "apps/api/src/auth/mod.rs",
    "apps/api/src/trust.rs",
    "apps/api/src/versions.rs",
    "apps/api/src/uploads.rs",
    "apps/desktop/src-tauri/src/deeplink/mod.rs",
    "apps/desktop/src-tauri/src/auth/mod.rs",
    "apps/desktop/src-tauri/src/launch/mod.rs",
    "apps/desktop/src-tauri/src/updater/mod.rs",
    "apps/desktop/src-tauri/src/social/crypto/mod.rs",
    "apps/desktop/src-tauri/capabilities/main.json",
    "apps/desktop/src-tauri/tauri.conf.json",
    ".github/workflows/ci.yml",
    "deny.toml",
    ".gitleaks.toml",
    "Cargo.lock",
    "pnpm-lock.yaml",
];

#[derive(Debug)]
pub struct Rule {
    pub line: usize,
    pub pattern: String,
    pub owners: Vec<String>,
    pub security: bool,
    matchers: Vec<GlobMatcher>,
}

fn glob(pattern: &str) -> Result<GlobMatcher> {
    Ok(GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .with_context(|| format!("invalid pattern {pattern:?}"))?
        .compile_matcher())
}

impl Rule {
    /// CODEOWNERS (gitignore-style) semantics: a pattern with a `/` before its
    /// end is anchored at the root, otherwise it matches at any depth; a
    /// pattern naming a directory covers everything below it; a final `*`
    /// component matches files directly in that directory only.
    fn new(line: usize, pattern: &str, owners: Vec<String>, security: bool) -> Result<Self> {
        let trimmed = pattern.trim_end_matches('/');
        let anchored = trimmed.contains('/');
        let core = trimmed.trim_start_matches('/');
        let base = if anchored {
            core.to_owned()
        } else {
            format!("**/{core}")
        };
        let last = core.rsplit('/').next().unwrap_or(core);
        let mut matchers = Vec::new();
        if !pattern.ends_with('/') {
            matchers.push(glob(&base)?);
        }
        if !last.contains(['*', '?']) {
            matchers.push(glob(&format!("{base}/**"))?);
        }
        Ok(Rule {
            line,
            pattern: pattern.to_owned(),
            owners,
            security,
            matchers,
        })
    }

    pub fn matches(&self, path: &str) -> bool {
        self.matchers.iter().any(|m| m.is_match(path))
    }
}

pub fn parse(text: &str) -> Result<Vec<Rule>> {
    let mut rules = Vec::new();
    let mut security = false;
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        if raw.starts_with(SECURITY_MARKER) {
            security = true;
        }
        let (content, comment) = raw.split_once('#').unwrap_or((raw, ""));
        let content = content.trim();
        if content.is_empty() {
            continue;
        }
        if security && !comment.contains("team: security") {
            bail!(
                "line {line}: only `# team: security` rules may follow the security section (keep it last)"
            );
        }
        let mut parts = content.split_whitespace();
        let Some(pattern) = parts.next() else {
            continue;
        };
        let owners: Vec<String> = parts.map(str::to_owned).collect();
        if owners.is_empty() {
            bail!("line {line}: {pattern} has no owner");
        }
        for o in &owners {
            let valid = o.strip_prefix('@').is_some_and(|name| {
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'/'))
            }) || (o.contains('@') && !o.starts_with('@'));
            if !valid {
                bail!("line {line}: {o:?} is not a @user, @org/team or email owner");
            }
        }
        rules.push(Rule::new(line, pattern, owners, security)?);
    }
    Ok(rules)
}

/// The rule GitHub applies to `path`: the last one that matches.
pub fn owner_rule<'a>(rules: &'a [Rule], path: &str) -> Option<&'a Rule> {
    rules.iter().rev().find(|r| r.matches(path))
}

/// Every problem found: files without owners, security paths not owned by the
/// security section.
pub fn problems(rules: &[Rule], files: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for f in files {
        if owner_rule(rules, f).is_none() {
            out.push(format!("{f} has no code owner"));
        }
    }
    for p in SECURITY_CRITICAL {
        match owner_rule(rules, p) {
            Some(r) if r.security => {}
            Some(r) => out.push(format!(
                "{p} is security-critical but its last matching rule is line {} ({} {}), outside the security section",
                r.line,
                r.pattern,
                r.owners.join(" ")
            )),
            None => out.push(format!("{p} is security-critical but has no code owner")),
        }
    }
    out
}

pub fn check(root: &Path) -> Result<bool> {
    let text = std::fs::read_to_string(root.join(".github/CODEOWNERS"))
        .context("reading .github/CODEOWNERS")?;
    let rules = parse(&text)?;
    let out = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z"])
        .output()
        .context("running git ls-files")?;
    if !out.status.success() {
        bail!("git ls-files failed");
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    let files: Vec<&str> = listing.split('\0').filter(|f| !f.is_empty()).collect();
    let problems = problems(&rules, &files);
    for p in &problems {
        println!("{p}");
    }
    println!(
        "{} files, {} rules, {} problems",
        files.len(),
        rules.len(),
        problems.len()
    );
    Ok(problems.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(text: &str) -> Vec<Rule> {
        parse(text).unwrap()
    }

    #[test]
    fn pattern_semantics() {
        let r = rules(
            "/*  @a\n/apps/api/  @b\n*.md  @c\n/docs/*  @d\ndocs/ @e\n/apps/api/migrations  @f\n",
        );
        let owner = |p: &str| owner_rule(&r, p).map(|r| r.owners[0].clone());
        assert_eq!(owner("Cargo.toml").as_deref(), Some("@a"));
        assert_eq!(owner("apps/api/src/lib.rs").as_deref(), Some("@b"));
        assert_eq!(owner("apps/api/README.md").as_deref(), Some("@c"));
        // `/docs/*` covers direct children only; `docs/` covers any depth, anywhere.
        assert_eq!(owner("docs/x.txt").as_deref(), Some("@e"));
        assert_eq!(owner("a/docs/x/y.rs").as_deref(), Some("@e"));
        // A directory without a trailing slash still covers its contents; last match wins.
        assert_eq!(owner("apps/api/migrations/1.sql").as_deref(), Some("@f"));
        // `/*` does not cover nested files.
        assert_eq!(owner("apps/other/x.rs"), None);
    }

    #[test]
    fn owners_are_required_and_well_formed() {
        assert!(parse("/x\n").is_err());
        assert!(parse("/x owner\n").is_err());
        assert!(parse("/x @\n").is_err());
        parse("/x @user @org/team dev@example.com\n").unwrap();
    }

    #[test]
    fn security_paths_must_resolve_to_the_security_section() {
        let text = format!(
            "/* @a\n/crates/ @a\n{SECURITY_MARKER}\n/crates/vgames-core/ @sec # team: security\n"
        );
        let r = rules(&text);
        let p = problems(&r, &["Cargo.toml", "crates/vgames-core/src/lib.rs"]);
        // Every other security path is reported, vgames-core is fine.
        assert!(
            !p.iter()
                .any(|x| x.starts_with("crates/vgames-core/src/lib.rs"))
        );
        assert!(p.iter().any(|x| x.starts_with("deny.toml")));
        // A later non-security rule would take vgames-core away from security: refused.
        assert!(parse(&format!("{text}/crates/ @a\n")).is_err());
        // A security path covered only by a rule before the section: reported.
        let r = rules(&format!(
            "/crates/ @a\n{SECURITY_MARKER}\n/deny.toml @sec # team: security\n"
        ));
        let p = problems(&r, &[]);
        assert!(
            p.iter()
                .any(|x| x.starts_with("crates/vgames-core/src/lib.rs") && x.contains("outside"))
        );
    }

    #[test]
    fn repository_codeowners_is_complete() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        assert!(check(&root).unwrap(), "see the problems printed above");
    }
}
