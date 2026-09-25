//! `cargo xtask changelog release <version>` (08-release §3.3): folds every
//! fragment into `CHANGELOG.md`, adds the release to the cumulative
//! `changelog-user.json`, writes the `latest.json` notes, and deletes the
//! consumed fragments. The player-facing entries come from the release-notes
//! agent's validated output; they are checked again here with the same rules.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::changelog::{self, Audience, ChangeType, Component, Fragment};

/// Shown when a release has no player-facing entry (08-release §3.3).
pub const GENERIC_NOTE: &str = "Stability and performance improvements.";
pub const USER_FORMAT: &str = "vgames.changelog/1";

#[derive(Debug, Deserialize)]
pub struct NotesFile {
    pub source: String,
    #[serde(default)]
    pub model: Option<String>,
    pub entries: Vec<NotesEntry>,
    #[serde(default)]
    pub dropped: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NotesEntry {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
    pub source_fragments: Vec<String>,
}

/// The same guards as the agent: cite only user launcher fragments, pass the lint.
pub fn check_entries(entries: &[NotesEntry], fragments: &[Fragment]) -> Vec<String> {
    let mut problems = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let at = format!(
            "entries[{i}] ({:?})",
            e.text.chars().take(60).collect::<String>()
        );
        let Some(kind) = ChangeType::parse(&e.kind) else {
            problems.push(format!("{at}: unknown type {:?}", e.kind));
            continue;
        };
        if e.source_fragments.is_empty() {
            problems.push(format!("{at}: cites no fragment"));
        }
        for slug in &e.source_fragments {
            match fragments.iter().find(|f| &f.slug == slug) {
                None => problems.push(format!("{at}: cites unknown fragment {slug:?}")),
                Some(f) if f.audience != Audience::User => {
                    problems.push(format!("{at}: cites internal fragment {slug:?}"))
                }
                Some(f) if f.component != Component::Launcher => {
                    problems.push(format!("{at}: cites a non-launcher fragment {slug:?}"))
                }
                Some(_) => {}
            }
        }
        for p in changelog::lint_user_text(&e.text, kind) {
            problems.push(format!("{at}: {p}"));
        }
    }
    problems
}

fn component_title(c: Component) -> &'static str {
    match c {
        Component::Launcher => "Launcher",
        Component::Admin => "Admin",
        Component::Server => "Server",
    }
}

fn type_title(t: ChangeType) -> &'static str {
    match t {
        ChangeType::Added => "Added",
        ChangeType::Changed => "Changed",
        ChangeType::Fixed => "Fixed",
        ChangeType::Removed => "Removed",
        ChangeType::Security => "Security",
    }
}

/// The `CHANGELOG.md` section for this release: every fragment, grouped by component and type.
pub fn changelog_section(version: &str, date: &str, fragments: &[Fragment]) -> String {
    let mut out = format!("## {version} ({date})\n");
    let mut sorted: Vec<&Fragment> = fragments.iter().collect();
    sorted.sort_by(|a, b| {
        (a.component, a.kind, a.audience, &a.slug).cmp(&(b.component, b.kind, b.audience, &b.slug))
    });
    let mut current: Option<(Component, ChangeType)> = None;
    for f in sorted {
        if current.map(|c| c.0) != Some(f.component) {
            let _ = write!(out, "\n### {}\n", component_title(f.component));
            current = None;
        }
        if current != Some((f.component, f.kind)) {
            let _ = write!(out, "\n#### {}\n\n", type_title(f.kind));
            current = Some((f.component, f.kind));
        }
        let text = f.text.split_whitespace().collect::<Vec<_>>().join(" ");
        match f.audience {
            Audience::User => {
                let _ = writeln!(out, "- {text}");
            }
            Audience::Internal => {
                let _ = writeln!(out, "- (internal) {text}");
            }
        }
    }
    if fragments.is_empty() {
        out.push_str("\nNo changes recorded.\n");
    }
    out
}

/// Inserts `section` above the previous releases of `CHANGELOG.md`.
pub fn prepend_changelog(existing: Option<&str>, section: &str) -> String {
    const HEADER: &str =
        "# Changelog\n\nWritten by the agents that made each change (08-release §3).\n";
    let rest = existing
        .map(|s| s.strip_prefix(HEADER).unwrap_or(s).trim_start().to_owned())
        .unwrap_or_default();
    format!("{HEADER}\n{section}\n{rest}").trim_end().to_owned() + "\n"
}

/// Adds this release (newest first) to the cumulative `changelog-user.json`.
pub fn update_user_json(
    previous: Option<&str>,
    version: &str,
    date: &str,
    entries: &[NotesEntry],
) -> Result<String> {
    let mut releases: Vec<Value> = match previous {
        Some(text) => {
            let v: Value = serde_json::from_str(text).context("previous changelog-user.json")?;
            if v.get("format").and_then(Value::as_str) != Some(USER_FORMAT) {
                bail!("previous changelog-user.json is not {USER_FORMAT}");
            }
            v.get("releases")
                .and_then(Value::as_array)
                .cloned()
                .context("previous changelog-user.json has no releases")?
        }
        None => Vec::new(),
    };
    if releases
        .iter()
        .any(|r| r.get("version").and_then(Value::as_str) == Some(version))
    {
        bail!("changelog-user.json already has release {version}");
    }
    let entries: Vec<Value> = entries
        .iter()
        .map(|e| json!({ "type": e.kind, "text": e.text }))
        .collect();
    releases.insert(
        0,
        json!({ "version": version, "date": date, "entries": entries }),
    );
    let doc = json!({ "format": USER_FORMAT, "releases": releases });
    Ok(serde_json::to_string_pretty(&doc)? + "\n")
}

/// `latest.json` `notes`: one bullet per entry, or the generic line.
pub fn latest_notes(entries: &[NotesEntry]) -> String {
    if entries.is_empty() {
        return GENERIC_NOTE.to_owned();
    }
    entries
        .iter()
        .map(|e| format!("- {}", e.text))
        .collect::<Vec<_>>()
        .join("\n")
}

fn valid_date(d: &str) -> bool {
    let b = d.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

pub struct ReleaseArgs<'a> {
    pub version: &'a str,
    pub date: &'a str,
    pub notes: &'a Path,
    pub previous_user_json: Option<&'a Path>,
    pub out_dir: &'a Path,
}

/// Runs the whole assembly. `fragments` are the valid fragments with their file names.
pub fn run(root: &Path, fragments: &[(String, Fragment)], args: &ReleaseArgs<'_>) -> Result<()> {
    semver::Version::parse(args.version).context("version must be semver, e.g. 0.4.0")?;
    if !valid_date(args.date) {
        bail!("date must be YYYY-MM-DD");
    }
    let notes: NotesFile = serde_json::from_str(
        &std::fs::read_to_string(args.notes)
            .with_context(|| format!("reading {}", args.notes.display()))?,
    )
    .context("notes file")?;
    let only: Vec<Fragment> = fragments.iter().map(|(_, f)| f.clone()).collect();
    let problems = check_entries(&notes.entries, &only);
    if !problems.is_empty() {
        bail!(
            "the release notes fail the guards:\n{}",
            problems.join("\n")
        );
    }

    let changelog_path = root.join("CHANGELOG.md");
    let existing = std::fs::read_to_string(&changelog_path).ok();
    let section = changelog_section(args.version, args.date, &only);
    std::fs::write(
        &changelog_path,
        prepend_changelog(existing.as_deref(), &section),
    )?;

    let previous = args
        .previous_user_json
        .map(|p| std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display())))
        .transpose()?;
    std::fs::create_dir_all(args.out_dir)?;
    let user_json = update_user_json(previous.as_deref(), args.version, args.date, &notes.entries)?;
    std::fs::write(args.out_dir.join("changelog-user.json"), user_json)?;
    std::fs::write(
        args.out_dir.join("latest-notes.txt"),
        latest_notes(&notes.entries) + "\n",
    )?;

    for (name, _) in fragments {
        std::fs::remove_file(root.join(".changes").join(name))
            .with_context(|| format!("deleting .changes/{name}"))?;
    }
    println!(
        "release {}: {} fragments folded, {} player entries, {} dropped ({}{})",
        args.version,
        fragments.len(),
        notes.entries.len(),
        notes.dropped.len(),
        notes.source,
        notes.model.map(|m| format!(", {m}")).unwrap_or_default()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(
        slug: &str,
        audience: Audience,
        component: Component,
        kind: ChangeType,
        text: &str,
    ) -> Fragment {
        Fragment {
            slug: slug.into(),
            audience,
            component,
            kind,
            text: text.into(),
        }
    }

    fn fragments() -> Vec<Fragment> {
        vec![
            frag(
                "resume",
                Audience::User,
                Component::Launcher,
                ChangeType::Fixed,
                "Downloads now resume after your computer restarts.",
            ),
            frag(
                "engine",
                Audience::Internal,
                Component::Launcher,
                ChangeType::Changed,
                "Refactored the download engine.",
            ),
            frag(
                "bulk",
                Audience::User,
                Component::Admin,
                ChangeType::Added,
                "Admins can now withdraw several versions at once.",
            ),
        ]
    }

    fn entry(slugs: &[&str], text: &str) -> NotesEntry {
        NotesEntry {
            kind: "fixed".into(),
            text: text.into(),
            source_fragments: slugs.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn guards_refuse_promoted_internal_and_admin_fragments() {
        let f = fragments();
        assert!(
            check_entries(
                &[entry(
                    &["resume"],
                    "Downloads now resume after your computer restarts."
                )],
                &f
            )
            .is_empty()
        );
        let p = check_entries(&[entry(&["engine"], "Downloads start faster now.")], &f);
        assert!(p.iter().any(|x| x.contains("internal fragment")), "{p:?}");
        let p = check_entries(&[entry(&["bulk"], "Admins can withdraw versions.")], &f);
        assert!(p.iter().any(|x| x.contains("non-launcher")), "{p:?}");
        let p = check_entries(&[entry(&["resume"], "Refactored downloads.")], &f);
        assert!(p.iter().any(|x| x.contains("technical")), "{p:?}");
    }

    #[test]
    fn internal_only_release_gets_the_generic_line() {
        assert_eq!(latest_notes(&[]), GENERIC_NOTE);
        let json = update_user_json(None, "0.4.0", "2026-10-02", &[]).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["releases"][0]["entries"], json!([]));
    }

    #[test]
    fn user_json_is_cumulative_newest_first_and_refuses_duplicates() {
        let e = [entry(
            &["resume"],
            "Downloads now resume after your computer restarts.",
        )];
        let first = update_user_json(None, "0.4.0", "2026-10-02", &e).unwrap();
        let second = update_user_json(Some(&first), "0.5.0", "2026-11-02", &[]).unwrap();
        let v: Value = serde_json::from_str(&second).unwrap();
        assert_eq!(v["format"], USER_FORMAT);
        assert_eq!(v["releases"][0]["version"], "0.5.0");
        assert_eq!(v["releases"][1]["entries"][0]["type"], "fixed");
        assert!(update_user_json(Some(&second), "0.5.0", "2026-11-03", &[]).is_err());
        assert!(
            update_user_json(Some("{\"format\":\"other\"}"), "0.6.0", "2026-12-01", &[]).is_err()
        );
    }

    #[test]
    fn changelog_groups_all_audiences() {
        let section = changelog_section("0.4.0", "2026-10-02", &fragments());
        let launcher = section.find("### Launcher").unwrap();
        let admin = section.find("### Admin").unwrap();
        assert!(launcher < admin);
        assert!(section.contains("- (internal) Refactored the download engine."));
        assert!(section.contains("#### Fixed\n\n- Downloads now resume"));
        let doc = prepend_changelog(None, &section);
        let again = prepend_changelog(Some(&doc), &changelog_section("0.5.0", "2026-11-02", &[]));
        assert!(again.find("## 0.5.0").unwrap() < again.find("## 0.4.0").unwrap());
        assert_eq!(again.matches("# Changelog").count(), 1);
    }

    #[test]
    fn dates_are_iso() {
        assert!(valid_date("2026-10-02"));
        assert!(!valid_date("2026-1-02"));
        assert!(!valid_date("02/10/2026"));
    }
}
