//! `cargo xtask` — repository automation. Owner: Agent 5.
//!
//! - `changelog lint`                 validate every `.changes/*.md` fragment (08-release §3.2)
//! - `changelog check --base <rev>`   the PR gate: lint, and require a new fragment in this PR
//! - `changelog lint-text --type T`   lint one player-facing text from stdin (release-notes guard)
//! - `casefold <CaseFolding.txt>`     regenerate vgames-core's Unicode simple case folding table
//! - `codeowners check`             every tracked file has an owner; security paths stay with security
//! - `openapi check`                  fail if the API's generated spec drifts from openapi/openapi.yaml
//! - `updater sign | manifest | verify` version-bound updater signatures and `latest.json` (08-release §2)

mod casefold;
mod changelog;
mod codeowners;
mod release;
mod updater;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::changelog::{Audience, ChangeType, Component, Fragment};

#[derive(Parser)]
#[command(name = "cargo xtask", about = "vgames repository automation")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Changelog fragments (.changes/*.md).
    Changelog {
        #[command(subcommand)]
        command: ChangelogCmd,
    },
    /// .github/CODEOWNERS checks.
    Codeowners {
        #[command(subcommand)]
        command: CodeownersCmd,
    },
    /// OpenAPI contract checks.
    Openapi {
        #[command(subcommand)]
        command: OpenapiCmd,
    },
    /// Regenerate crates/vgames-core/src/paths/casefold.rs from Unicode's CaseFolding.txt.
    Casefold {
        /// Path to CaseFolding.txt (https://www.unicode.org/Public/UCD/latest/ucd/CaseFolding.txt).
        input: PathBuf,
    },
    /// Launcher-update artifacts (release workflow).
    Updater {
        #[command(subcommand)]
        command: UpdaterCmd,
    },
}

#[derive(Subcommand)]
enum UpdaterCmd {
    /// Sign updater artifacts (writes FILE.sig) with `version:<V>` in the trusted comment.
    Sign {
        /// The release version (semver), e.g. 0.4.0.
        #[arg(long)]
        version: String,
        /// Environment variable holding the Tauri updater private key (content or path).
        #[arg(long, default_value = "TAURI_SIGNING_PRIVATE_KEY")]
        key_env: String,
        /// Environment variable holding its password.
        #[arg(long, default_value = "TAURI_SIGNING_PRIVATE_KEY_PASSWORD")]
        password_env: String,
        files: Vec<PathBuf>,
    },
    /// Write latest.json after verifying every artifact and its signed version.
    Manifest {
        #[arg(long)]
        version: String,
        /// latest.json notes (`dist/latest-notes.txt` from `changelog release`).
        #[arg(long)]
        notes: PathBuf,
        /// RFC 3339 publication time.
        #[arg(long)]
        pub_date: String,
        /// Where the artifacts are downloaded from (the GitHub release's download URL).
        #[arg(long)]
        url_base: String,
        /// TARGET=FILE, e.g. windows-x86_64=vgames_0.4.0_x64-setup.exe (FILE.sig next to it).
        #[arg(long = "artifact", required = true)]
        artifacts: Vec<String>,
        /// Updater public key (base64, as in tauri.conf.json); default: from tauri.conf.json.
        #[arg(long)]
        pubkey: Option<String>,
        #[arg(long, default_value = "latest.json")]
        out: PathBuf,
    },
    /// Verify latest.json against the artifacts in a directory.
    Verify {
        #[arg(long, default_value = "latest.json")]
        latest: PathBuf,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        #[arg(long)]
        pubkey: Option<String>,
    },
}

fn tauri_conf(root: &Path) -> PathBuf {
    root.join("apps/desktop/src-tauri/tauri.conf.json")
}

fn run_updater(root: &Path, command: UpdaterCmd) -> Result<bool> {
    match command {
        UpdaterCmd::Sign {
            version,
            key_env,
            password_env,
            files,
        } => {
            let key = std::env::var(&key_env).with_context(|| format!("{key_env} is not set"))?;
            let password = std::env::var(&password_env).ok();
            updater::sign(&version, &key, password.as_deref(), &files)?;
        }
        UpdaterCmd::Manifest {
            version,
            notes,
            pub_date,
            url_base,
            artifacts,
            pubkey,
            out,
        } => {
            let pk = updater::load_public_key(pubkey.as_deref(), &tauri_conf(root))?;
            let artifacts = updater::parse_artifacts(&artifacts)?;
            let latest = updater::manifest(&updater::ManifestArgs {
                version: &version,
                notes: &notes,
                pub_date: &pub_date,
                url_base: &url_base,
                pubkey: &pk,
                artifacts: &artifacts,
            })?;
            let mut json = serde_json::to_vec_pretty(&latest)?;
            json.push(b'\n');
            std::fs::write(&out, json).with_context(|| format!("writing {}", out.display()))?;
            println!(
                "wrote {} ({} platforms)",
                out.display(),
                latest.platforms.len()
            );
        }
        UpdaterCmd::Verify {
            latest,
            dir,
            pubkey,
        } => {
            let pk = updater::load_public_key(pubkey.as_deref(), &tauri_conf(root))?;
            let n = updater::verify(&latest, &dir, &pk)?;
            println!("{} verified: {n} platforms", latest.display());
        }
    }
    Ok(true)
}

#[derive(Subcommand)]
enum CodeownersCmd {
    /// Every tracked file matches a rule; security-critical paths resolve to the security section.
    Check,
}

#[derive(Subcommand)]
enum OpenapiCmd {
    /// Compare the API's generated OpenAPI document with openapi/openapi.yaml
    /// (Agent 1's drift test, apps/api/tests/openapi_contract.rs).
    Check,
}

#[derive(Subcommand)]
enum ChangelogCmd {
    /// Lint every fragment in .changes/.
    Lint,
    /// PR gate: lint, require at least one fragment added since `base`, and warn when
    /// launcher code changed but no fragment is `audience: user, component: launcher`.
    Check {
        /// The PR's base commit (e.g. github.event.pull_request.base.sha).
        #[arg(long)]
        base: String,
    },
    /// Lint one player-facing text read from stdin (exit 1 with the problems on stdout).
    LintText {
        #[arg(long = "type", default_value = "added")]
        kind: String,
    },
    /// Print every fragment as JSON (input of the release-notes agent).
    Export,
    /// Release assembly (08-release §3.3): CHANGELOG.md, changelog-user.json,
    /// latest.json notes; deletes the consumed fragments.
    Release {
        /// Semver of the release, e.g. 0.4.0.
        version: String,
        /// The release-notes agent's validated output (scripts/release-notes).
        #[arg(long)]
        notes: PathBuf,
        /// Release date, YYYY-MM-DD.
        #[arg(long)]
        date: String,
        /// The previous release's changelog-user.json (it is cumulative).
        #[arg(long)]
        previous_user_json: Option<PathBuf>,
        /// Where to write changelog-user.json and latest-notes.txt.
        #[arg(long, default_value = "dist")]
        out_dir: PathBuf,
    },
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Parses every fragment; prints problems; returns the valid ones.
fn lint_all(root: &Path) -> Result<(Vec<Fragment>, usize)> {
    let dir = root.join(".changes");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n != "README.md")
        .collect();
    names.sort();
    let mut ok = Vec::new();
    let mut failures = 0;
    for name in names {
        let content = std::fs::read_to_string(dir.join(&name))
            .with_context(|| format!("reading .changes/{name}"))?;
        match changelog::parse_fragment(&name, &content) {
            Ok(f) => ok.push(f),
            Err(errors) => {
                failures += 1;
                for e in errors {
                    // GitHub annotation when running in Actions, plain text otherwise.
                    if std::env::var_os("GITHUB_ACTIONS").is_some() {
                        println!("::error file=.changes/{name}::{e}");
                    } else {
                        println!(".changes/{name}: {e}");
                    }
                }
            }
        }
    }
    Ok((ok, failures))
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn check(root: &Path, base: &str) -> Result<bool> {
    let (fragments, failures) = lint_all(root)?;
    let diff = git(
        root,
        &[
            "diff",
            "--name-status",
            "--no-renames",
            &format!("{base}...HEAD"),
        ],
    )?;
    let mut added = Vec::new();
    let mut deleted_fragments = 0;
    let mut touches_launcher = false;
    let mut touches_changelog = false;
    for line in diff.lines() {
        let mut parts = line.split('\t');
        let (Some(status), Some(path)) = (parts.next(), parts.next()) else {
            continue;
        };
        if path.starts_with("apps/desktop/") {
            touches_launcher = true;
        }
        if path == "CHANGELOG.md" {
            touches_changelog = true;
        }
        if let Some(name) = path.strip_prefix(".changes/")
            && name != "README.md"
            && !name.contains('/')
        {
            match status {
                "A" => added.push(name.trim_end_matches(".md").to_owned()),
                "D" => deleted_fragments += 1,
                _ => {}
            }
        }
    }
    let mut ok = failures == 0;
    let release_assembly = deleted_fragments > 0 && touches_changelog;
    if added.is_empty() && !release_assembly {
        ok = false;
        println!(
            "::error::This PR adds no changelog fragment. Add .changes/<slug>.md (see .changes/README.md and AGENTS.md)."
        );
    }
    let launcher_user = fragments.iter().any(|f| {
        added.contains(&f.slug)
            && f.audience == Audience::User
            && f.component == Component::Launcher
    });
    if touches_launcher && !launcher_user && !added.is_empty() {
        println!(
            "::warning::apps/desktop changed but no fragment is `audience: user` + `component: launcher`. \
             Confirm players cannot see or feel this change; otherwise write a user fragment."
        );
    }
    Ok(ok)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let root = repo_root();
    let result = match cli.command {
        Cmd::Changelog {
            command: ChangelogCmd::Lint,
        } => lint_all(&root).map(|(ok, failures)| {
            println!("{} fragments valid, {failures} invalid", ok.len());
            failures == 0
        }),
        Cmd::Changelog {
            command: ChangelogCmd::Check { base },
        } => check(&root, &base),
        Cmd::Changelog {
            command: ChangelogCmd::LintText { kind },
        } => (|| {
            let kind = ChangeType::parse(&kind).context("unknown --type")?;
            let text = std::io::read_to_string(std::io::stdin())?;
            let errors = changelog::lint_user_text(&text, kind);
            for e in &errors {
                println!("{e}");
            }
            Ok(errors.is_empty())
        })(),
        Cmd::Codeowners {
            command: CodeownersCmd::Check,
        } => codeowners::check(&root),
        Cmd::Changelog {
            command: ChangelogCmd::Export,
        } => lint_all(&root).and_then(|(fragments, failures)| {
            if failures > 0 {
                anyhow::bail!("fix the invalid fragments first");
            }
            let list: Vec<serde_json::Value> = fragments
                .iter()
                .map(|f| {
                    serde_json::json!({
                        "slug": f.slug,
                        "audience": f.audience.as_str(),
                        "component": f.component.as_str(),
                        "type": f.kind.as_str(),
                        "text": f.text,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&list)?);
            Ok(true)
        }),
        Cmd::Changelog {
            command:
                ChangelogCmd::Release {
                    version,
                    notes,
                    date,
                    previous_user_json,
                    out_dir,
                },
        } => lint_all(&root).and_then(|(fragments, failures)| {
            if failures > 0 {
                anyhow::bail!("fix the invalid fragments first");
            }
            let named: Vec<(String, Fragment)> = fragments
                .into_iter()
                .map(|f| (format!("{}.md", f.slug), f))
                .collect();
            release::run(
                &root,
                &named,
                &release::ReleaseArgs {
                    version: &version,
                    date: &date,
                    notes: &notes,
                    previous_user_json: previous_user_json.as_deref(),
                    out_dir: &out_dir,
                },
            )
            .map(|()| true)
        }),
        Cmd::Casefold { input } => casefold::generate(&input, &root).map(|()| true),
        Cmd::Updater { command } => run_updater(&root, command),
        Cmd::Openapi {
            command: OpenapiCmd::Check,
        } => Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .current_dir(&root)
            .args(["test", "-p", "vgames-api", "--test", "openapi_contract"])
            .status()
            .context("running cargo test")
            .map(|s| s.success()),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
