//! Updater rules that do not depend on Tauri (08-release §2, §3.3), so they are
//! unit-tested on their own:
//!
//! - only a strictly greater semver is an update (never a downgrade);
//! - update endpoints and download URLs are HTTPS (loopback HTTP only in debug builds);
//! - `changelog-user.json` is capped at 1 MiB, strictly validated, and plain text;
//! - "What's new" shows the releases in `(installed, new]`;
//! - checks never run while a game runs or a download is active, and installs
//!   wait until every download has reached a checkpoint (paused or finished).

use std::collections::{HashMap, HashSet};

use semver::Version;
use serde::{Deserialize, Serialize};
use specta::Type;
use url::Url;
use uuid::Uuid;

/// Largest `changelog-user.json` accepted.
pub const MAX_CHANGELOG_BYTES: usize = 1024 * 1024;
pub const CHANGELOG_FORMAT: &str = "vgames.changelog/1";
/// Shown for a release without player-facing entries (08-release §3.3).
pub const GENERIC_NOTE: &str = "Stability and performance improvements.";
const MAX_RELEASES: usize = 1000;
const MAX_ENTRIES: usize = 200;
const MAX_TEXT_CHARS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("{0} is not an HTTPS URL")]
    NotHttps(String),
    #[error("the changelog is larger than 1 MiB")]
    ChangelogTooLarge,
    #[error("the changelog is not valid vgames.changelog/1: {0}")]
    ChangelogInvalid(String),
}

/// Strictly greater: an equal or older version is never offered.
pub fn is_update(current: &Version, remote: &Version) -> bool {
    remote > current
}

/// HTTPS only. Debug builds may use `http://localhost` / `127.0.0.1` for local tests.
pub fn require_https(url: &Url, allow_loopback_http: bool) -> Result<(), PolicyError> {
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    match url.scheme() {
        "https" => Ok(()),
        "http" if allow_loopback_http && loopback => Ok(()),
        _ => Err(PolicyError::NotHttps(format!(
            "{}://{}",
            url.scheme(),
            url.host_str().unwrap_or_default()
        ))),
    }
}

/// Kind of a player-facing change, in the order the dialog groups them.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Type,
)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    Added,
    Changed,
    Fixed,
    Removed,
    Security,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ChangeEntry {
    #[serde(rename = "type")]
    pub kind: ChangeType,
    /// Plain text; the UI renders it as text, never as HTML or Markdown.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ReleaseNotes {
    pub version: String,
    pub date: String,
    pub entries: Vec<ChangeEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangelogFile {
    format: String,
    releases: Vec<ReleaseNotes>,
}

fn plain_text(s: &str) -> bool {
    let n = s.chars().count();
    (1..=MAX_TEXT_CHARS).contains(&n) && !s.chars().any(char::is_control)
}

fn iso_date(d: &str) -> bool {
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

/// Parses `changelog-user.json` (already capped by the caller at [`MAX_CHANGELOG_BYTES`]).
pub fn parse_changelog(bytes: &[u8]) -> Result<Vec<ReleaseNotes>, PolicyError> {
    if bytes.len() > MAX_CHANGELOG_BYTES {
        return Err(PolicyError::ChangelogTooLarge);
    }
    let invalid = |why: String| PolicyError::ChangelogInvalid(why);
    let file: ChangelogFile = serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))?;
    if file.format != CHANGELOG_FORMAT {
        return Err(invalid("unknown format".into()));
    }
    if file.releases.len() > MAX_RELEASES {
        return Err(invalid("too many releases".into()));
    }
    for r in &file.releases {
        Version::parse(&r.version).map_err(|_| invalid(format!("bad version {:?}", r.version)))?;
        if !iso_date(&r.date) {
            return Err(invalid(format!("bad date for {}", r.version)));
        }
        if r.entries.len() > MAX_ENTRIES {
            return Err(invalid(format!("too many entries in {}", r.version)));
        }
        if let Some(e) = r.entries.iter().find(|e| !plain_text(&e.text)) {
            return Err(invalid(format!(
                "entry of {} is empty, too long or has control characters: {:?}",
                r.version,
                e.text.chars().take(40).collect::<String>()
            )));
        }
    }
    Ok(file.releases)
}

/// What the "What's new" dialog shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct WhatsNew {
    /// Releases in `(installed, new]`, newest first. A release with no entries
    /// is shown as [`GENERIC_NOTE`].
    pub releases: Vec<ReleaseNotes>,
    /// True when the changelog could not be used and `notes` from `latest.json` are shown.
    pub from_latest_notes: bool,
}

/// Selects the releases in `(installed, new]`, newest first.
pub fn select(releases: &[ReleaseNotes], installed: &Version, new: &Version) -> Vec<ReleaseNotes> {
    let mut picked: Vec<(Version, ReleaseNotes)> = releases
        .iter()
        .filter_map(|r| Version::parse(&r.version).ok().map(|v| (v, r.clone())))
        .filter(|(v, _)| v > installed && v <= new)
        .collect();
    picked.sort_by(|a, b| b.0.cmp(&a.0));
    picked.dedup_by(|a, b| a.0 == b.0);
    picked.into_iter().map(|(_, r)| r).collect()
}

/// Builds the dialog content from the changelog, or from `latest.json` notes when
/// the changelog is unavailable or does not mention the new version.
pub fn whats_new(
    changelog: Option<&[ReleaseNotes]>,
    latest_notes: Option<&str>,
    installed: &Version,
    new: &Version,
) -> WhatsNew {
    if let Some(releases) = changelog {
        let selected = select(releases, installed, new);
        if selected.iter().any(|r| r.version == new.to_string()) {
            return WhatsNew {
                releases: selected,
                from_latest_notes: false,
            };
        }
    }
    let entries = latest_notes
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().trim_start_matches("- ").trim())
        .filter(|l| plain_text(l) && *l != GENERIC_NOTE)
        .map(|text| ChangeEntry {
            kind: ChangeType::Changed,
            text: text.to_owned(),
        })
        .take(MAX_ENTRIES)
        .collect();
    WhatsNew {
        releases: vec![ReleaseNotes {
            version: new.to_string(),
            date: String::new(),
            entries,
        }],
        from_latest_notes: true,
    }
}

/// Why an update check or install must wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Blocked {
    GameRunning,
    DownloadsActive,
}

/// Games and installs in progress, fed from the launcher's event bus.
#[derive(Debug, Default)]
pub struct Activity {
    games: HashSet<(Uuid, Uuid)>,
    /// Install → is it at a checkpoint (paused)?
    installs: HashMap<(Uuid, Uuid), bool>,
}

impl Activity {
    pub fn game_started(&mut self, package: (Uuid, Uuid)) {
        self.games.insert(package);
    }
    pub fn game_stopped(&mut self, package: (Uuid, Uuid)) {
        self.games.remove(&package);
    }
    pub fn install_progress(&mut self, package: (Uuid, Uuid), paused: bool) {
        self.installs.insert(package, paused);
    }
    pub fn install_finished(&mut self, package: (Uuid, Uuid)) {
        self.installs.remove(&package);
    }

    /// Why nothing may happen now: a running game first, then active downloads.
    pub fn blocked(&self) -> Option<Blocked> {
        if !self.games.is_empty() {
            Some(Blocked::GameRunning)
        } else if self.installs.values().any(|paused| !paused) {
            Some(Blocked::DownloadsActive)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn only_strictly_newer_versions_are_updates() {
        assert!(is_update(&v("0.4.0"), &v("0.4.1")));
        assert!(!is_update(&v("0.4.1"), &v("0.4.1")));
        assert!(!is_update(&v("0.4.1"), &v("0.4.0")));
        assert!(!is_update(&v("1.0.0"), &v("1.0.0-rc.1")));
    }

    #[test]
    fn https_only() {
        let u = |s: &str| Url::parse(s).unwrap();
        require_https(&u("https://github.com/x/latest.json"), false).unwrap();
        assert!(require_https(&u("http://github.com/x/latest.json"), false).is_err());
        assert!(require_https(&u("http://127.0.0.1:8080/latest.json"), false).is_err());
        require_https(&u("http://127.0.0.1:8080/latest.json"), true).unwrap();
        assert!(require_https(&u("http://evil.example/latest.json"), true).is_err());
        assert!(require_https(&u("file:///tmp/latest.json"), true).is_err());
    }

    fn changelog() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "format": "vgames.changelog/1",
            "releases": [
                { "version": "0.6.0", "date": "2026-12-01", "entries": [] },
                { "version": "0.5.0", "date": "2026-11-01", "entries": [
                    { "type": "added", "text": "You can now pin favorite packages to the top of your library." } ] },
                { "version": "0.4.0", "date": "2026-10-01", "entries": [
                    { "type": "fixed", "text": "Downloads now resume after your computer restarts." } ] }
            ]
        }))
        .unwrap()
    }

    #[test]
    fn selects_releases_between_installed_and_new() {
        let releases = parse_changelog(&changelog()).unwrap();
        let w = whats_new(Some(&releases), None, &v("0.4.0"), &v("0.6.0"));
        assert!(!w.from_latest_notes);
        let versions: Vec<_> = w.releases.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(versions, ["0.6.0", "0.5.0"]);
        assert!(
            w.releases[0].entries.is_empty(),
            "shown as the generic line"
        );
    }

    #[test]
    fn falls_back_to_latest_notes() {
        let releases = parse_changelog(&changelog()).unwrap();
        // The changelog does not know 0.7.0 yet.
        let w = whats_new(
            Some(&releases),
            Some("- Faster library.\n- Fixed a crash on start."),
            &v("0.6.0"),
            &v("0.7.0"),
        );
        assert!(w.from_latest_notes);
        assert_eq!(w.releases[0].entries.len(), 2);
        assert_eq!(w.releases[0].entries[0].text, "Faster library.");
        // No changelog at all, generic notes only.
        let w = whats_new(None, Some(GENERIC_NOTE), &v("0.6.0"), &v("0.7.0"));
        assert!(w.releases[0].entries.is_empty());
    }

    #[test]
    fn rejects_oversized_and_malformed_changelogs() {
        assert_eq!(
            parse_changelog(&vec![b' '; MAX_CHANGELOG_BYTES + 1]),
            Err(PolicyError::ChangelogTooLarge)
        );
        let bad = |value: serde_json::Value| parse_changelog(&serde_json::to_vec(&value).unwrap());
        for doc in [
            serde_json::json!({ "format": "vgames.changelog/2", "releases": [] }),
            serde_json::json!({ "format": "vgames.changelog/1", "releases": [], "extra": 1 }),
            serde_json::json!({ "format": "vgames.changelog/1", "releases": [{ "version": "latest", "date": "2026-10-01", "entries": [] }] }),
            serde_json::json!({ "format": "vgames.changelog/1", "releases": [{ "version": "1.0.0", "date": "Oct 1", "entries": [] }] }),
            serde_json::json!({ "format": "vgames.changelog/1", "releases": [{ "version": "1.0.0", "date": "2026-10-01", "entries": [{ "type": "hack", "text": "x" }] }] }),
            serde_json::json!({ "format": "vgames.changelog/1", "releases": [{ "version": "1.0.0", "date": "2026-10-01", "entries": [{ "type": "added", "text": "a\u{1b}[31mred" }] }] }),
            serde_json::json!({ "format": "vgames.changelog/1", "releases": [{ "version": "1.0.0", "date": "2026-10-01", "entries": [{ "type": "added", "text": "" }] }] }),
        ] {
            assert!(bad(doc.clone()).is_err(), "{doc}");
        }
        assert!(parse_changelog(b"not json").is_err());
    }

    #[test]
    fn activity_gates_checks_and_installs() {
        let p = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut a = Activity::default();
        assert_eq!(a.blocked(), None);
        a.install_progress(p, false);
        assert_eq!(a.blocked(), Some(Blocked::DownloadsActive));
        a.install_progress(p, true);
        assert_eq!(a.blocked(), None, "a paused download is at a checkpoint");
        a.game_started(p);
        assert_eq!(a.blocked(), Some(Blocked::GameRunning));
        a.game_stopped(p);
        a.install_finished(p);
        assert_eq!(a.blocked(), None);
    }
}
