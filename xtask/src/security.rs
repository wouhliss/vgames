//! `cargo xtask security check` (A5-T11, docs/security/test-matrix.md): the
//! configuration that security invariants rest on, which no other test pins.
//!
//! - **Launcher WebView** (invariant 4; "malicious admin UI content" in
//!   01-security §1): the production CSP runs scripts from the app only (no
//!   inline, no eval), connects to nothing but IPC, loads nothing from a remote
//!   origin and allows no plugins, frames, forms or `<base>`; the prototype is
//!   frozen and the asset protocol, which would read local files, is off.
//! - **Launcher capabilities** (invariant 4): windows get Tauri's `core`
//!   defaults and vgames' own `allow-*` commands only, never a plugin
//!   permission (fs, http, shell, dialog, opener…) and never a remote URL.
//! - **Workflows** ("compromised CI"): a job that reads a signing secret or
//!   mints OIDC tokens runs in the approval-gated `release` environment, a job
//!   that reads any other secret runs in some environment, no secret is set at
//!   workflow level, and nothing runs on `pull_request_target` or
//!   `workflow_run` (code from forks next to secrets).

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;
use serde_yaml_ng::Value as Yaml;

/// Secrets that sign what users run: updater artifacts, the runtime catalog,
/// Windows and Apple code signatures (docs/security/release.md).
const SIGNING_SECRETS: &[&str] = &[
    "TAURI_SIGNING_PRIVATE_KEY",
    "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
    "VGAMES_RUNTIME_CATALOG_KEY",
    "VGAMES_RUNTIME_CATALOG_KEY_PASSWORD",
    "WINDOWS_CERTIFICATE",
    "WINDOWS_CERTIFICATE_PASSWORD",
];
const SIGNING_PREFIXES: &[&str] = &["APPLE_"];

fn is_signing_secret(name: &str) -> bool {
    SIGNING_SECRETS.contains(&name) || SIGNING_PREFIXES.iter().any(|p| name.starts_with(p))
}

// ---------------------------------------------------------------------------
// Launcher CSP

fn sources(csp: &serde_json::Map<String, Value>, directive: &str) -> Option<Vec<String>> {
    csp.get(directive)
        .and_then(Value::as_str)
        .map(|s| s.split_whitespace().map(str::to_owned).collect())
}

/// A source that stays inside the app: a keyword other than the unsafe ones,
/// `data:`, a custom scheme (`ipc:`, `vgimg:`), or Tauri's
/// `http://<scheme>.localhost` form of one. Never a remote origin or `*`.
fn local_source(source: &str) -> bool {
    if source.starts_with('\'') {
        return !source.starts_with("'unsafe-") && source != "'wasm-unsafe-eval'";
    }
    if let Some(host) = source.strip_prefix("http://") {
        return host
            .strip_suffix(".localhost")
            .is_some_and(|name| !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase()));
    }
    source.ends_with(':')
        && source.len() > 1
        && !matches!(
            source,
            "http:" | "https:" | "ws:" | "wss:" | "blob:" | "filesystem:"
        )
}

pub fn csp_problems(conf: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let null = Value::Null;
    let security = conf.pointer("/app/security").unwrap_or(&null);
    let Some(csp) = security.get("csp").and_then(Value::as_object) else {
        return vec!["tauri.conf.json: app.security.csp must be set (as an object)".to_owned()];
    };
    let exactly = |directive: &str, want: &[&str], out: &mut Vec<String>| {
        let got = sources(csp, directive);
        if got
            .as_deref()
            .map(|g| g.iter().map(String::as_str).collect::<Vec<_>>())
            != Some(want.to_vec())
        {
            out.push(format!(
                "tauri.conf.json: CSP {directive} must be exactly {:?}, found {got:?}",
                want.join(" ")
            ));
        }
    };
    exactly("default-src", &["'self'"], &mut out);
    exactly("script-src", &["'self'"], &mut out);
    exactly("object-src", &["'none'"], &mut out);
    exactly("base-uri", &["'none'"], &mut out);
    exactly("form-action", &["'none'"], &mut out);
    exactly("frame-ancestors", &["'none'"], &mut out);
    match sources(csp, "connect-src") {
        Some(c) if c.iter().all(|s| s == "ipc:" || s == "http://ipc.localhost") => {}
        other => out.push(format!(
            "tauri.conf.json: CSP connect-src may only reach IPC (ipc: http://ipc.localhost), found {other:?}"
        )),
    }
    for (directive, value) in csp {
        for source in value.as_str().unwrap_or_default().split_whitespace() {
            if !local_source(source) {
                out.push(format!(
                    "tauri.conf.json: CSP {directive} allows {source}: only the app's own sources may be listed"
                ));
            }
        }
    }
    if security.get("freezePrototype") != Some(&Value::Bool(true)) {
        out.push("tauri.conf.json: app.security.freezePrototype must be true".to_owned());
    }
    if security.pointer("/assetProtocol/enable") == Some(&Value::Bool(true)) {
        out.push(
            "tauri.conf.json: the asset protocol must stay off (it gives the WebView file access)"
                .to_owned(),
        );
    }
    if !matches!(
        security.get("dangerousDisableAssetCspModification"),
        None | Some(Value::Null | Value::Bool(false))
    ) {
        out.push(
            "tauri.conf.json: dangerousDisableAssetCspModification must not be set".to_owned(),
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Launcher capabilities

pub fn capability_problems(file: &str, cap: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if cap.get("remote").is_some() {
        out.push(format!("{file}: remote URLs must never get IPC access"));
    }
    for permission in cap
        .get("permissions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = permission
            .as_str()
            .or_else(|| permission.get("identifier").and_then(Value::as_str))
            .unwrap_or_default();
        let core_default = id == "core:default"
            || id
                .strip_prefix("core:")
                .and_then(|rest| rest.strip_suffix(":default"))
                .is_some_and(|plugin| !plugin.is_empty() && !plugin.contains(':'));
        let own_command = id.strip_prefix("allow-").is_some_and(|c| {
            !c.is_empty() && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
        });
        if !(core_default || own_command) {
            out.push(format!(
                "{file}: permission {id:?} is not a core default or a vgames command \
                 (implement a typed command instead of granting a plugin)"
            ));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Workflows

fn secrets_in(text: &str) -> Vec<String> {
    let mut found: Vec<String> = text
        .match_indices("secrets.")
        .filter_map(|(i, _)| {
            let name: String = text
                .get(i + "secrets.".len()..)?
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect();
    found.sort();
    found.dedup();
    found
}

fn environment(job: &Yaml) -> Option<String> {
    match job.get("environment")? {
        Yaml::String(s) => Some(s.clone()),
        env => env.get("name").and_then(Yaml::as_str).map(str::to_owned),
    }
}

fn triggers(workflow: &Yaml) -> Vec<String> {
    // YAML 1.1 readers may parse the `on` key as `true`.
    let on = workflow
        .get("on")
        .or_else(|| workflow.as_mapping().and_then(|m| m.get(Yaml::Bool(true))));
    match on {
        Some(Yaml::String(s)) => vec![s.clone()],
        Some(Yaml::Sequence(seq)) => seq
            .iter()
            .filter_map(Yaml::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Yaml::Mapping(map)) => map
            .keys()
            .filter_map(Yaml::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

pub fn workflow_problems(file: &str, text: &str) -> Result<Vec<String>> {
    let workflow: Yaml =
        serde_yaml_ng::from_str(text).with_context(|| format!("{file} is not valid YAML"))?;
    let mut out = Vec::new();
    for trigger in triggers(&workflow) {
        if trigger == "pull_request_target" || trigger == "workflow_run" {
            out.push(format!(
                "{file}: `{trigger}` runs code from forks with access to secrets; use pull_request"
            ));
        }
    }
    if let Some(env) = workflow.get("env") {
        let names = secrets_in(&serde_yaml_ng::to_string(env)?);
        if !names.is_empty() {
            out.push(format!(
                "{file}: workflow-level env reads {names:?}; read secrets in the job that needs them"
            ));
        }
    }
    let jobs = workflow.get("jobs").and_then(Yaml::as_mapping);
    for (name, job) in jobs.into_iter().flatten() {
        let name = name.as_str().unwrap_or("?");
        let body = serde_yaml_ng::to_string(job)?;
        let mut secrets = secrets_in(&body);
        secrets.retain(|s| s != "GITHUB_TOKEN");
        let inherits = job.get("secrets").and_then(Yaml::as_str) == Some("inherit");
        let oidc = job
            .get("permissions")
            .and_then(|p| p.get("id-token"))
            .and_then(Yaml::as_str)
            == Some("write");
        let env = environment(job);
        let signing: Vec<&String> = secrets.iter().filter(|s| is_signing_secret(s)).collect();
        if (!signing.is_empty() || inherits || oidc) && env.as_deref() != Some("release") {
            let why = if oidc {
                "mints OIDC tokens (signatures, attestations)".to_owned()
            } else if inherits {
                "inherits every secret".to_owned()
            } else {
                format!("reads signing secrets {signing:?}")
            };
            out.push(format!(
                "{file}: job `{name}` {why} but runs in {} instead of the `release` environment",
                env.map_or("no environment".to_owned(), |e| format!("`{e}`"))
            ));
        } else if !secrets.is_empty() && env.is_none() {
            out.push(format!(
                "{file}: job `{name}` reads {secrets:?} without an environment (secrets live in environments)"
            ));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------

pub fn problems(root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let tauri = root.join("apps/desktop/src-tauri");
    let conf: Value = serde_json::from_str(
        &std::fs::read_to_string(tauri.join("tauri.conf.json"))
            .context("reading tauri.conf.json")?,
    )
    .context("tauri.conf.json is not JSON")?;
    out.extend(csp_problems(&conf));

    let mut caps: Vec<_> = std::fs::read_dir(tauri.join("capabilities"))
        .context("reading capabilities/")?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    caps.sort();
    for path in caps {
        let name = format!(
            "capabilities/{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        let cap: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)
            .with_context(|| format!("{name} is not JSON"))?;
        out.extend(capability_problems(&name, &cap));
    }

    let mut workflows: Vec<_> = std::fs::read_dir(root.join(".github/workflows"))
        .context("reading .github/workflows")?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
        .collect();
    workflows.sort();
    for path in &workflows {
        let name = format!(
            ".github/workflows/{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        out.extend(workflow_problems(&name, &std::fs::read_to_string(path)?)?);
    }
    Ok(out)
}

pub fn check(root: &Path) -> Result<bool> {
    let problems = problems(root)?;
    for p in &problems {
        println!("{p}");
    }
    println!("security gates: {} problems", problems.len());
    Ok(problems.is_empty())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use serde_json::json;

    fn root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    #[test]
    fn the_repository_passes() {
        let problems = problems(&root()).unwrap();
        assert!(problems.is_empty(), "{problems:#?}");
    }

    fn conf(csp: Value) -> Value {
        json!({ "app": { "security": { "csp": csp, "freezePrototype": true,
                                        "assetProtocol": { "enable": false } } } })
    }

    fn good_csp() -> Value {
        json!({
            "default-src": "'self'", "script-src": "'self'", "style-src": "'self'",
            "img-src": "'self' data: vgimg: http://vgimg.localhost",
            "connect-src": "ipc: http://ipc.localhost", "object-src": "'none'",
            "base-uri": "'none'", "form-action": "'none'", "frame-ancestors": "'none'"
        })
    }

    fn with(key: &str, value: &str) -> Value {
        let mut csp = good_csp();
        csp[key] = json!(value);
        conf(csp)
    }

    #[test]
    fn csp_rules() {
        assert!(csp_problems(&conf(good_csp())).is_empty());
        for (key, value) in [
            ("script-src", "'self' 'unsafe-inline'"),
            ("script-src", "'self' 'unsafe-eval'"),
            ("script-src", "'self' https://cdn.example.com"),
            (
                "connect-src",
                "ipc: http://ipc.localhost https://games.example.com",
            ),
            (
                "connect-src",
                "ipc: http://ipc.localhost ws://localhost:1420",
            ),
            ("img-src", "'self' https:"),
            ("img-src", "'self' *"),
            ("style-src", "'self' http://evil.example.com"),
            ("object-src", "'self'"),
            ("frame-ancestors", "*"),
            ("default-src", "'self' blob:"),
        ] {
            assert!(
                !csp_problems(&with(key, value)).is_empty(),
                "{key}: {value}"
            );
        }
        let mut missing = good_csp();
        missing.as_object_mut().unwrap().remove("base-uri");
        assert!(!csp_problems(&conf(missing)).is_empty());
        assert!(!csp_problems(&json!({ "app": { "security": {} } })).is_empty());

        let mut c = conf(good_csp());
        c["app"]["security"]["freezePrototype"] = json!(false);
        assert!(!csp_problems(&c).is_empty());
        let mut c = conf(good_csp());
        c["app"]["security"]["assetProtocol"]["enable"] = json!(true);
        assert!(!csp_problems(&c).is_empty());
        let mut c = conf(good_csp());
        c["app"]["security"]["dangerousDisableAssetCspModification"] = json!(true);
        assert!(!csp_problems(&c).is_empty());
    }

    #[test]
    fn capability_rules() {
        let cap = |permissions: Value| json!({ "identifier": "main", "windows": ["main"], "permissions": permissions });
        assert!(
            capability_problems(
                "c",
                &cap(json!([
                    "core:default",
                    "core:event:default",
                    "allow-app-info"
                ]))
            )
            .is_empty()
        );
        for bad in [
            json!(["fs:default"]),
            json!(["http:default"]),
            json!(["shell:allow-open"]),
            json!(["dialog:default"]),
            json!(["opener:default"]),
            json!(["core:webview:allow-create-webview-window"]),
            json!([{ "identifier": "fs:allow-read-file", "allow": [{ "path": "$HOME/**" }] }]),
            json!(["allow-"]),
        ] {
            assert!(
                !capability_problems("c", &cap(bad.clone())).is_empty(),
                "{bad}"
            );
        }
        let mut remote = cap(json!(["core:default"]));
        remote["remote"] = json!({ "urls": ["https://games.example.com/*"] });
        assert!(!capability_problems("c", &remote).is_empty());
    }

    #[test]
    fn workflow_rules() {
        let ok = "
on: { push: { tags: ['desktop-v*'] } }
jobs:
  build:
    environment: release
    permissions: { id-token: write }
    steps:
      - run: sign
        env: { KEY: '${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}', T: '${{ secrets.GITHUB_TOKEN }}' }
  bot:
    environment: { name: runtimes-bot }
    steps: [{ run: x, env: { K: '${{ secrets.RUNTIMES_BOT_PRIVATE_KEY }}' } }]
  test:
    steps: [{ run: cargo test, env: { T: '${{ secrets.GITHUB_TOKEN }}' } }]
";
        assert!(workflow_problems("w", ok).unwrap().is_empty());

        let bad = [
            // Signing secret outside `release`.
            "on: push\njobs:\n  b:\n    steps: [{ run: x, env: { K: '${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}' } }]\n",
            "on: push\njobs:\n  b:\n    environment: staging\n    steps: [{ run: x, env: { K: '${{ secrets.APPLE_ID }}' } }]\n",
            "on: push\njobs:\n  b:\n    environment: runtimes-bot\n    steps: [{ run: x, env: { K: '${{ secrets.VGAMES_RUNTIME_CATALOG_KEY }}' } }]\n",
            // OIDC or inherited secrets outside `release`.
            "on: push\njobs:\n  b:\n    permissions: { id-token: write }\n    steps: [{ run: x }]\n",
            "on: push\njobs:\n  b:\n    uses: ./.github/workflows/x.yml\n    secrets: inherit\n",
            // Any other secret without an environment.
            "on: push\njobs:\n  b:\n    steps: [{ run: x, env: { K: '${{ secrets.ANTHROPIC_API_KEY }}' } }]\n",
            // Secrets at workflow level.
            "on: push\nenv: { K: '${{ secrets.ANTHROPIC_API_KEY }}' }\njobs:\n  b:\n    steps: [{ run: x }]\n",
            // Fork code next to secrets.
            "on: pull_request_target\njobs:\n  b:\n    steps: [{ run: x }]\n",
            "on: [push, workflow_run]\njobs:\n  b:\n    steps: [{ run: x }]\n",
            "on:\n  workflow_run: { workflows: [CI] }\njobs:\n  b:\n    steps: [{ run: x }]\n",
        ];
        for text in bad {
            assert!(!workflow_problems("w", text).unwrap().is_empty(), "{text}");
        }
    }
}
