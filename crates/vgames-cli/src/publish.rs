//! `vgames publish`: packs a folder, uploads it, signs its manifest with a
//! publisher key and finalizes the version (02-package-format §6), with Agent 2's
//! publishing library (`vgames_transfer::upload::publish`).
//!
//! - Checked before anything is uploaded: the folder packs cleanly (no symlinks
//!   or special files), launch targets exist, and the key is in the server's
//!   current trust bundle (verified under the pinned root), not revoked, valid
//!   now and held by the signed-in user. The server checks the key again at
//!   finalize, but only after the whole upload.
//! - Resumable: the idempotency key, the created version and the upload sessions
//!   are kept in `0600` files in the config directory, outside the folder.
//!   Running the same command again continues; `--restart` aborts the
//!   unfinished version and starts over.
//! - Stops at `ready` (verified by the server, not released) unless `--publish`.

use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vgames_core::keyfile::KeyKind;
use vgames_core::sign::KeyId;
use vgames_core::trust::KeyStatus;
use vgames_pack::manifest::Execution;
use vgames_pack::scan::{FsReader, RejectReason, scan};
use vgames_pack::{PACK_SIZE, PackSource, Plan};
use vgames_proto::auth::Me;
use vgames_proto::discovery::ServerInfo;
use vgames_proto::packages::{AdminPackage, AdminPackagePage, Platform};
use vgames_proto::versions::{FinalizeRequest, UploadTarget, Version, VersionCreate, VersionState};
use vgames_transfer::download::RemoteError;
use vgames_transfer::upload::publish::{
    self, PublishApi, PublishControl, PublishError, PublishOptions, PublishPhase, PublishRequest,
};
use vgames_transfer::upload::{UploadControl, UploadPhase};

use crate::keys;
use crate::secret::PassphraseArgs;
use crate::server::{Api, Problem, ServerArgs, problem_code};
use crate::session::{self, Session};
use crate::trust_server::{current_bundle, pin_for};

#[derive(Args)]
#[command(after_help = "\
Examples:
  vgames publish ./build --server https://games.example.com --package my-game \\
      --platform windows-x86_64 --version-label 1.4.0 --key publisher.vgkey --execution launch.toml
  vgames publish ./build … --publish                           release it once verified
  vgames publish ./build … --passphrase-env PUB_PASS --json    in CI

Interrupted? Run the same command again: the upload resumes. --restart aborts the unfinished
version instead and starts over.

Without --publish the version stops at `ready`: verified by the server, not released to players.
Run the same command with --publish to release it (nothing is uploaded again).

--execution takes the manifest's launch settings as TOML or JSON, for example:
  [launch]
  default = \"game\"
  [[launch.targets]]
  id = \"game\"
  label = \"Play\"
  executable = \"bin/game.exe\"")]
pub struct PublishArgs {
    /// The folder to publish: its contents become the package's files.
    folder: PathBuf,
    #[command(flatten)]
    server: ServerArgs,
    /// Package id or slug.
    #[arg(long, value_name = "ID|SLUG")]
    package: String,
    /// windows-x86_64, windows-aarch64, linux-x86_64, linux-aarch64, macos-aarch64 or macos-x86_64.
    #[arg(long, value_parser = parse_platform)]
    platform: Platform,
    /// The version players see, 1–64 characters (e.g. 1.4.0).
    #[arg(long, value_name = "LABEL")]
    version_label: String,
    /// Your publisher key file.
    #[arg(long)]
    key: PathBuf,
    /// Launch targets, controllers, save locations and multiplayer (TOML or JSON, manifest field names).
    #[arg(long, value_name = "FILE")]
    execution: Option<PathBuf>,
    /// Release the version once the server has verified it.
    #[arg(long)]
    publish: bool,
    /// Abort the unfinished version of an earlier run and start over.
    #[arg(long)]
    restart: bool,
    /// Print the resulting version as JSON on stdout.
    #[arg(long)]
    json: bool,
    /// Root key file (public part) or base64 root public key, instead of the pinned one.
    #[arg(long)]
    root: Option<String>,
    #[command(flatten)]
    passphrase: PassphraseArgs,
}

fn parse_platform(s: &str) -> Result<Platform, String> {
    Platform::parse(s).ok_or_else(|| {
        format!(
            "unknown platform {s:?} (windows-x86_64, windows-aarch64, linux-x86_64, \
             linux-aarch64, macos-aarch64 or macos-x86_64)"
        )
    })
}

// ---------------------------------------------------------------------------
// Resume state

const STATE_FORMAT: &str = "vgames.cli-publish/1";

/// What a later run needs to continue: written before creating the version,
/// and again once the server has returned it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    format: String,
    origin: String,
    package_id: Uuid,
    platform: Platform,
    version_label: String,
    folder: PathBuf,
    idempotency_key: Uuid,
    version: Option<Version>,
}

struct StateFiles {
    state: PathBuf,
    /// The upload library's own resume record (sessions and confirmed offsets).
    packs: PathBuf,
}

impl StateFiles {
    fn new(origin: &str, package_id: Uuid, platform: Platform, label: &str) -> Result<Self> {
        let id = format!("{origin}\n{package_id}\n{}\n{label}", platform.as_str());
        let digest = vgames_core::Digest::of(id.as_bytes()).to_string();
        let name = digest.get(..32).unwrap_or(&digest);
        let dir = session::config_dir()?.join("publish");
        Ok(Self {
            state: dir.join(format!("{name}.json")),
            packs: dir.join(format!("{name}.packs.json")),
        })
    }

    fn load(&self) -> Result<Option<State>> {
        let bytes = match std::fs::read(&self.state) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.state.display())),
        };
        let state: State = serde_json::from_slice(&bytes)
            .ok()
            .filter(|s: &State| s.format == STATE_FORMAT)
            .with_context(|| {
                format!(
                    "{} is not a publish state file; pass --restart to start over",
                    self.state.display()
                )
            })?;
        Ok(Some(state))
    }

    fn save(&self, state: &State) -> Result<()> {
        session::write_private(&self.state, &serde_json::to_vec_pretty(state)?)
    }

    fn forget(&self) -> Result<()> {
        // The library's OS lock beside its record (it keeps two runs off one version).
        let lock = self.packs.with_extension("upload-lock");
        for path in [&self.state, &self.packs, &lock] {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// API boundary for the publishing library

struct CliApi {
    api: Api,
    /// `--publish`. Without it, `publish` sends nothing (see there).
    release: bool,
    /// The last error the API returned: the library keeps response bodies out of
    /// its own errors, so this is what tells the user why a step was refused.
    last_error: Mutex<Option<String>>,
}

impl CliApi {
    fn remote(&self, e: anyhow::Error) -> RemoteError {
        let (retryable, code) = match e.downcast_ref::<Problem>() {
            Some(p) => (
                p.status == 429 || p.status >= 500,
                (!p.code.is_empty()).then(|| p.code.clone()),
            ),
            // Network errors and timeouts; anything else (a malformed answer) is not.
            None => (e.downcast_ref::<reqwest::Error>().is_some(), None),
        };
        let message = format!("{e:#}");
        if let Ok(mut last) = self.last_error.lock() {
            *last = Some(message.clone());
        }
        RemoteError {
            retryable,
            code,
            message,
        }
    }

    fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|l| l.clone())
    }
}

impl PublishApi for CliApi {
    async fn create_version(
        &self,
        package_id: Uuid,
        request: VersionCreate,
        idempotency_key: Uuid,
    ) -> Result<Version, RemoteError> {
        let path = format!("/v1/admin/packages/{package_id}/versions");
        self.api
            .post_idempotent(&path, idempotency_key, &request)
            .await
            .map_err(|e| self.remote(e))
    }

    async fn get_version(&self, version_id: Uuid) -> Result<Version, RemoteError> {
        let path = format!("/v1/admin/versions/{version_id}");
        self.api.get(&path).await.map_err(|e| self.remote(e))
    }

    async fn pack_upload_target(
        &self,
        version_id: Uuid,
        pack: u32,
    ) -> Result<UploadTarget, RemoteError> {
        let path = format!("/v1/admin/versions/{version_id}/packs/{pack}/upload-session");
        self.api
            .post_no_body(&path)
            .await
            .map_err(|e| self.remote(e))
    }

    async fn manifest_upload_target(&self, version_id: Uuid) -> Result<UploadTarget, RemoteError> {
        let path = format!("/v1/admin/versions/{version_id}/manifest-upload");
        self.api
            .post_no_body(&path)
            .await
            .map_err(|e| self.remote(e))
    }

    async fn finalize(
        &self,
        version_id: Uuid,
        request: FinalizeRequest,
    ) -> Result<Version, RemoteError> {
        let path = format!("/v1/admin/versions/{version_id}/finalize");
        self.api
            .post(&path, &request)
            .await
            .map_err(|e| self.remote(e))
    }

    async fn publish(&self, version_id: Uuid) -> Result<Version, RemoteError> {
        if !self.release {
            // Without --publish nothing is sent: the version stays `ready`, which the
            // library then reports as `PublishError::State(Ready)` (handled in `run`).
            return self.get_version(version_id).await;
        }
        let path = format!("/v1/admin/versions/{version_id}/publish");
        self.api
            .post_no_body(&path)
            .await
            .map_err(|e| self.remote(e))
    }
}

// ---------------------------------------------------------------------------
// Checks before uploading

/// Scans `folder` into the source the upload streams from.
fn source(folder: &Path) -> Result<PackSource> {
    let scanned = scan(folder).with_context(|| format!("scanning {}", folder.display()))?;
    if !scanned.rejected.is_empty() {
        let mut msg = format!(
            "{} contains entries that cannot be packed:",
            folder.display()
        );
        for r in scanned.rejected.iter().take(20) {
            let why = match r.reason {
                RejectReason::Symlink => "a symbolic link",
                RejectReason::SpecialFile => "not a regular file or folder",
                RejectReason::NotUtf8 => "a name that is not UTF-8",
            };
            let _ = write!(msg, "\n  {}: {why}", r.path);
        }
        if let Some(more) = scanned.rejected.len().checked_sub(20).filter(|n| *n > 0) {
            let _ = write!(msg, "\n  … and {more} more");
        }
        bail!(msg);
    }
    if scanned.files.is_empty() {
        bail!("{} has no files to publish", folder.display());
    }
    let plan = Plan::new(scanned.files, scanned.directories)
        .with_context(|| format!("{} cannot be published", folder.display()))?;
    let packing = plan.raw_packing(PACK_SIZE).context("laying out packs")?;
    Ok(PackSource::new(
        Arc::new(plan),
        Arc::new(packing),
        Arc::new(FsReader::new(folder)),
    ))
}

/// `--execution`, with every launch target's executable present in the folder.
/// The full manifest rules are checked again when the manifest is built.
fn execution(path: Option<&Path>, plan: &Plan) -> Result<Execution> {
    let Some(path) = path else {
        return Ok(Execution::default());
    };
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let bad = || format!("{} is not valid launch settings", path.display());
    let execution: Execution = if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
    {
        toml::from_str(&text).with_context(bad)?
    } else {
        serde_json::from_str(&text).with_context(bad)?
    };
    if let Some(launch) = &execution.launch {
        for target in &launch.targets {
            if !plan.files().iter().any(|f| f.path == target.executable) {
                bail!(
                    "launch target {:?}: {:?} is not a file in the folder",
                    target.id,
                    target.executable
                );
            }
        }
    }
    Ok(execution)
}

async fn resolve_package(api: &Api, package: &str) -> Result<Uuid> {
    if let Ok(id) = Uuid::parse_str(package) {
        let found: AdminPackage =
            api.get(&format!("/v1/admin/packages/{id}"))
                .await
                .map_err(|e| match problem_code(&e) {
                    Some((404, _)) => anyhow!("no package {id} on this server"),
                    _ => e,
                })?;
        return Ok(found.detail.summary.id);
    }
    let is_slug = !package.is_empty()
        && package.len() <= 64
        && package
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !is_slug {
        bail!("--package {package:?} is neither a package id nor a slug");
    }
    let mut cursor: Option<String> = None;
    // Bounded: 50 pages of 200 matches, however many cursors the server hands out.
    for _ in 0..50 {
        let mut path = format!("/v1/admin/packages?limit=200&q={package}");
        if let Some(c) = &cursor {
            let c: String = url::form_urlencoded::byte_serialize(c.as_bytes()).collect();
            let _ = write!(path, "&cursor={c}");
        }
        let page: AdminPackagePage = api.get(&path).await?;
        if let Some(p) = page.items.iter().find(|p| p.detail.summary.slug == package) {
            return Ok(p.detail.summary.id);
        }
        match page.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    bail!("no package with slug {package:?} on this server")
}

/// The key must be trusted by the server's current bundle, valid now and held by `me`.
async fn check_key(
    session: &mut Session,
    info: &ServerInfo,
    root: Option<&str>,
    key_id: &KeyId,
    me: Uuid,
) -> Result<()> {
    let pin = pin_for(session, root, info)?;
    let Some(bundle) = current_bundle(&session.api, &pin, info.server_id).await? else {
        bail!(
            "the server has no trust bundle yet; its owner publishes one with `vgames trust publish`"
        );
    };
    session.update_pin(&bundle.pin)?;
    let version = bundle.state.version();
    match bundle.state.key_status(key_id) {
        KeyStatus::Revoked => {
            bail!("publisher key {key_id} is revoked in the server's trust bundle (v{version})")
        }
        KeyStatus::Unknown => bail!(
            "publisher key {key_id} is not in the server's trust bundle (v{version}); \
             ask the owner to add it"
        ),
        KeyStatus::Trusted(p) => {
            if p.holder_user_id != me {
                bail!(
                    "publisher key {key_id} ({}) is held by another account of this server",
                    p.label
                );
            }
            let now = keys::now()?;
            if now < p.not_before || now >= p.not_after {
                bail!(
                    "publisher key {key_id} is valid from {} to {}, not now",
                    p.not_before,
                    p.not_after
                );
            }
        }
    }
    Ok(())
}

/// `--restart`: aborts the version of an earlier run (learning its id by
/// replaying the create when the first answer was lost).
async fn abort(api: &Api, state: &State, create: &VersionCreate) -> Result<()> {
    let version: Version = match &state.version {
        Some(v) => v.clone(),
        None => {
            let path = format!("/v1/admin/packages/{}/versions", state.package_id);
            api.post_idempotent(&path, state.idempotency_key, create)
                .await?
        }
    };
    match api
        .delete(&format!("/v1/admin/versions/{}", version.id))
        .await
    {
        Ok(()) => eprintln!("Aborted the unfinished version {}.", version.id),
        Err(e) if matches!(problem_code(&e), Some((404 | 409, _))) => {
            eprintln!(
                "Version {} can no longer be aborted; starting a new one.",
                version.id
            );
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The command

pub async fn run(args: PublishArgs) -> Result<()> {
    if args.version_label.trim().is_empty() || args.version_label.chars().count() > 64 {
        bail!("--version-label must be 1–64 characters");
    }
    let folder = std::fs::canonicalize(&args.folder)
        .with_context(|| format!("{} not found", args.folder.display()))?;
    if !folder.is_dir() {
        bail!("{} is not a folder", folder.display());
    }
    let key_file = keys::load(&args.key)?;
    if key_file.kind != KeyKind::Publisher {
        bail!(
            "{} is a {} key, not a publisher key",
            args.key.display(),
            key_file.kind.as_str()
        );
    }

    let origin = args.server.origin()?;
    let mut session = Session::open(&args.server).await?;
    let info: ServerInfo = session.api.get("/.well-known/vgames.json").await?;
    if let Some(creds) = &session.creds
        && creds.server_id != info.server_id
    {
        bail!(
            "{origin} now answers as another server than at `vgames login`; refusing to continue"
        );
    }
    let me: Me = session.api.get("/v1/me").await?;
    let package_id = resolve_package(&session.api, &args.package).await?;
    let create = VersionCreate {
        platform: args.platform,
        version_label: args.version_label.clone(),
    };

    let files = StateFiles::new(
        origin.as_str(),
        package_id,
        args.platform,
        &args.version_label,
    )?;
    let earlier = if args.restart {
        if let Ok(Some(old)) = files.load() {
            abort(&session.api, &old, &create).await?;
        }
        files.forget()?;
        None
    } else {
        files.load()?
    };
    if let Some(s) = &earlier
        && s.folder != folder
    {
        bail!(
            "an earlier run publishes {} {} from {}; use that folder, or pass --restart",
            args.package,
            args.version_label,
            s.folder.display()
        );
    }
    // Where an earlier run stopped, as the server sees it now.
    let current: Option<Version> = match earlier.as_ref().and_then(|s| s.version.as_ref()) {
        Some(v) => Some(
            session
                .api
                .get(&format!("/v1/admin/versions/{}", v.id))
                .await?,
        ),
        None => None,
    };

    let scan_root = folder.clone();
    let source = tokio::task::spawn_blocking(move || source(&scan_root))
        .await
        .context("scanning the folder")??;
    let execution = execution(args.execution.as_deref(), &source.plan)?;
    eprintln!(
        "{}: {} files, {}, {} packs.",
        folder.display(),
        source.plan.files().len(),
        bytes(source.plan.files().iter().map(|f| f.size).sum()),
        source.packing.pack_count()
    );

    // The key is needed only until the manifest is signed.
    let signing_key = if current
        .as_ref()
        .is_none_or(|v| v.state == VersionState::Uploading)
    {
        check_key(
            &mut session,
            &info,
            args.root.as_deref(),
            &key_file.key_id,
            me.user.id,
        )
        .await?;
        Some(keys::unlock(
            &args.key,
            KeyKind::Publisher,
            &args.passphrase,
        )?)
    } else {
        None
    };

    let mut state = match earlier {
        Some(s) => s,
        None => {
            let s = State {
                format: STATE_FORMAT.to_owned(),
                origin: origin.to_string(),
                package_id,
                platform: args.platform,
                version_label: args.version_label.clone(),
                folder,
                idempotency_key: Uuid::now_v7(),
                version: None,
            };
            files.save(&s)?;
            s
        }
    };
    let api = Arc::new(CliApi {
        api: session.api,
        release: args.publish,
        last_error: Mutex::new(None),
    });
    let options = PublishOptions::default();
    let control = PublishControl::new(UploadControl::new());
    let cancel = control.upload.cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\nStopping…");
            cancel.cancel();
        }
    });

    let version = match state.version.clone() {
        Some(v) => v,
        None => {
            let v = publish::create_version(
                api.as_ref(),
                info.server_id,
                package_id,
                create.clone(),
                state.idempotency_key,
                &options,
                &control,
            )
            .await
            .map_err(|e| failure(e, &api, &files, &args))?;
            state.version = Some(v.clone());
            // Recorded before uploading: a later run resumes this version.
            files.save(&state)?;
            v
        }
    };
    let version_id = version.id;
    eprintln!(
        "Version {version_id} (#{}) of {} for {}.",
        version.sequence,
        args.package,
        args.platform.as_str()
    );
    let reporter = tokio::spawn(report(control.clone(), args.publish));
    let request = PublishRequest {
        version,
        server_id: info.server_id,
        package_id,
        version_create: create,
        execution,
        source,
        resume_path: files.packs.clone(),
        signing_key,
    };
    let result = publish::run(request, api.clone(), &options, &control).await;
    // The reporter ends on the final phase; do not wait on it for long.
    let _ = tokio::time::timeout(Duration::from_secs(1), reporter).await;
    match result {
        Ok(version) => {
            files.forget()?;
            done(&version, &args, "Published")
        }
        // Stopped at `ready` on purpose (see `CliApi::publish`). The state is kept,
        // so the same command with --publish releases this version.
        Err(PublishError::State(VersionState::Ready)) if !args.publish => {
            let version = api
                .get_version(version_id)
                .await
                .map_err(|e| anyhow!(e.message))?;
            done(&version, &args, "Verified and ready, not released")?;
            if !args.json {
                eprintln!("Run the same command with --publish to release it.");
            }
            Ok(())
        }
        Err(e) => Err(failure(e, &api, &files, &args)),
    }
}

/// The error to show, with the server's own explanation when it gave one. A
/// version that cannot continue is forgotten, so the next run creates a new one.
fn failure(e: PublishError, api: &CliApi, files: &StateFiles, args: &PublishArgs) -> anyhow::Error {
    let terminal = matches!(
        e,
        PublishError::VerificationFailed
            | PublishError::Identity
            | PublishError::State(
                VersionState::Failed | VersionState::Aborted | VersionState::Yanked
            )
    );
    let mut msg = e.to_string();
    if let Some(server) = api.last_error() {
        let _ = write!(msg, "\n  {server}");
    }
    if matches!(e, PublishError::Manifest) && args.execution.is_some() {
        msg.push_str("\n  Check the launch settings passed with --execution.");
    }
    if terminal {
        match files.forget() {
            Ok(()) => msg.push_str("\n  The next run starts a new version."),
            Err(forget) => {
                let _ = write!(
                    msg,
                    "\n  Could not reset the local state ({forget:#}); pass --restart."
                );
            }
        }
    } else {
        msg.push_str("\n  Run the same command again to continue.");
    }
    anyhow!(msg)
}

fn done(version: &Version, args: &PublishArgs, what: &str) -> Result<()> {
    if args.json {
        println!("{}", serde_json::to_string_pretty(version)?);
    } else {
        println!(
            "{what}: {} {} for {} (version {}, #{}).",
            args.package,
            version.version_label,
            version.platform.as_str(),
            version.id,
            version.sequence
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Progress

fn phase_text(phase: PublishPhase, releasing: bool) -> Option<&'static str> {
    match phase {
        PublishPhase::Uploading => Some("Uploading packs…"),
        PublishPhase::Signing => Some("Signing the manifest…"),
        PublishPhase::UploadingManifest => Some("Uploading the manifest…"),
        PublishPhase::Finalizing => Some("Finalizing…"),
        PublishPhase::Verifying => Some("The server is verifying every chunk…"),
        PublishPhase::Publishing if releasing => Some("Releasing…"),
        _ => None,
    }
}

/// Phase lines on stderr, plus a live line (terminal) or one every 10 s (logs).
async fn report(control: PublishControl, releasing: bool) {
    let tty = std::io::stderr().is_terminal();
    let mut phases = control.progress();
    let upload = control.upload.progress();
    let mut tick = tokio::time::interval(if tty {
        Duration::from_millis(250)
    } else {
        Duration::from_secs(10)
    });
    let mut last = None;
    let mut in_line = false;
    loop {
        tokio::select! {
            changed = phases.changed() => if changed.is_err() { break },
            _ = tick.tick() => {}
        }
        let now = *phases.borrow();
        if last != Some(now.phase) {
            if in_line {
                eprintln!();
                in_line = false;
            }
            if let Some(text) = phase_text(now.phase, releasing) {
                eprintln!("{text}");
            }
            last = Some(now.phase);
        }
        let line = match now.phase {
            PublishPhase::Uploading => {
                let u = upload.borrow().clone();
                (u.phase == UploadPhase::Uploading && u.bytes_total > 0).then(|| {
                    format!(
                        "  {} of {} ({}), {}/s, {} in flight",
                        bytes(u.bytes_confirmed),
                        bytes(u.bytes_total),
                        percent(u.bytes_confirmed as f64 / u.bytes_total as f64),
                        bytes(u.bytes_per_second.max(0.0) as u64),
                        match u.active_packs {
                            1 => "1 pack".to_owned(),
                            n => format!("{n} packs"),
                        }
                    )
                })
            }
            PublishPhase::Verifying => now
                .verification
                .map(|f| format!("  {} verified", percent(f64::from(f)))),
            PublishPhase::Published | PublishPhase::Failed | PublishPhase::Cancelled => break,
            _ => None,
        };
        if let Some(line) = line {
            if tty {
                eprint!("\r\x1b[2K{line}");
                in_line = true;
            } else {
                eprintln!("{line}");
            }
        }
    }
    if in_line {
        eprintln!();
    }
}

fn percent(f: f64) -> String {
    format!("{:.0}%", (f * 100.0).clamp(0.0, 100.0))
}

fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS.get(unit).unwrap_or(&"TiB"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_platforms() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
        assert_eq!(percent(0.371), "37%");
        assert_eq!(percent(2.0), "100%");
        assert_eq!(parse_platform("linux-x86_64"), Ok(Platform::LinuxX86_64));
        assert!(parse_platform("linux").is_err());
    }
}
