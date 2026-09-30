//! `vgames trust publish` and `vgames trust re-sign`: the server side of the key
//! pipeline (01-security §3.2–§3.3).
//!
//! Both verify locally before sending anything: bundles are checked against the
//! root key pinned at `vgames login` (or `--root`), and re-signing only ever
//! re-attests a manifest digest that the old key provably signed.

use std::collections::BTreeMap;
use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;
use vgames_core::keyfile::KeyKind;
use vgames_core::sign::{Context as SigContext, Envelope, KeyId, PublicKey};
use vgames_core::trust::{
    KeyStatus, RootPin, SignedBundle, TrustBundle, VerifiedBundle, verify_bundle,
};
use vgames_proto::discovery::ServerInfo;

use crate::keys;
use crate::secret::PassphraseArgs;
use crate::server::{Api, ServerArgs, problem_code};
use crate::session::Session;
use crate::trust::root_key;

#[derive(Args)]
#[command(after_help = "\
Examples:
  vgames trust publish --server https://games.example.com --signed bundle.signed.json
  vgames trust publish --server https://games.example.com --signed bundle.signed.json --root root.pub
  vgames trust publish --server https://games.example.com --signed bundle.signed.json --root <new root> --new-identity

Owner only. The bundle is verified locally first: signed by the server's root key (the one
pinned at `vgames login`, or --root), for this server, and newer than the server's bundle.

--new-identity is for a server whose root key was replaced because the old one was lost or
stolen (docs/security/runbooks.md §5): the server must already advertise --root, the bundle
must be signed by it and newer than the server's current one, whose old signature is no
longer checked. Launchers treat such a server as a new identity that players add again.")]
pub struct PublishArgs {
    #[command(flatten)]
    server: ServerArgs,
    /// The signed `{bundle, signature}` JSON from `vgames trust sign`.
    #[arg(long)]
    signed: PathBuf,
    /// Root key file (public part) or base64 root public key, instead of the pinned one.
    #[arg(long)]
    root: Option<String>,
    /// The server's root key was replaced on purpose (lost or stolen): publish under --root
    /// without verifying the server's current bundle, which the old root signed.
    #[arg(long, requires = "root")]
    new_identity: bool,
}

#[derive(Args)]
#[command(after_help = "\
Examples:
  vgames trust re-sign --server https://games.example.com --from 9f2c…(32 hex) --key new.vgkey --dry-run
  vgames trust re-sign --server https://games.example.com --from 9f2c… --key new.vgkey --previous v4.signed.json
  vgames trust re-sign --server … --from 9f2c… --key new.vgkey --finalized-before 2026-10-01T12:00:00Z --yes --passphrase-env PUB_PASS

After revoking a key (or before it expires), swaps the signature of every version signed by
it for one by your new publisher key. Manifest bytes never change. Each old signature is
verified under the old key first, so only manifests the old key really signed are re-signed.
If the key was stolen, pass --finalized-before with the time of the theft and review the list:
versions finalized later may have been signed by the thief.

The new key must already be trusted in the server's current bundle, and held by you.")]
pub struct ReSignArgs {
    #[command(flatten)]
    server: ServerArgs,
    /// Key id (32 hex) of the old publisher key.
    #[arg(long, value_name = "KEY_ID")]
    from: String,
    /// Your new publisher key file.
    #[arg(long)]
    key: PathBuf,
    /// A bundle (signed JSON or raw) that still lists the old key, if the current one does not.
    #[arg(long)]
    previous: Option<PathBuf>,
    /// Only versions finalized before this time (RFC 3339), e.g. when a key was stolen.
    #[arg(long, value_name = "TIME")]
    finalized_before: Option<String>,
    /// Root key file (public part) or base64 root public key, instead of the pinned one.
    #[arg(long)]
    root: Option<String>,
    /// List what would be re-signed and stop.
    #[arg(long)]
    dry_run: bool,
    /// Do not ask for confirmation.
    #[arg(long)]
    yes: bool,
    #[command(flatten)]
    passphrase: PassphraseArgs,
}

/// The root to verify bundles against: `--root`, else the pin from `vgames login`.
pub(crate) fn pin_for(session: &Session, root: Option<&str>, info: &ServerInfo) -> Result<RootPin> {
    let advertised = PublicKey::from_base64(&info.root_public_key)
        .context("the server advertises an invalid root key")?;
    let pin = match (root, &session.creds) {
        (Some(r), _) => RootPin::new(root_key(r)?),
        (None, Some(creds)) => creds.pin()?,
        (None, None) => bail!("pass --root (no stored session to take the pinned root key from)"),
    };
    if advertised != pin.root && Some(&advertised) != pin.next_root.as_ref() {
        bail!(
            "the server advertises root key {}, but the expected one is {}; refusing to continue",
            advertised.fingerprint(),
            pin.root.fingerprint()
        );
    }
    Ok(pin)
}

/// The server's current bundle, verified. `None` when it has none yet.
pub(crate) async fn current_bundle(
    api: &Api,
    pin: &RootPin,
    server_id: Uuid,
) -> Result<Option<VerifiedBundle>> {
    let signed: SignedBundle = match api.get("/v1/trust/bundle").await {
        Ok(s) => s,
        Err(e) if matches!(problem_code(&e), Some((404, _))) => return Ok(None),
        Err(e) => return Err(e),
    };
    let bytes = signed.bundle_bytes()?;
    let verified = verify_bundle(&bytes, &signed.signature, pin, None, server_id)
        .context("the server's current trust bundle does not verify under the pinned root key")?;
    Ok(Some(verified))
}

/// The version of the server's current bundle, read without verifying it. Only for
/// `--new-identity`: the old root's chain cannot verify under the new root, but versions
/// must still go up.
async fn current_version_unverified(api: &Api) -> Result<Option<u64>> {
    let signed: SignedBundle = match api.get("/v1/trust/bundle").await {
        Ok(s) => s,
        Err(e) if matches!(problem_code(&e), Some((404, _))) => return Ok(None),
        Err(e) => return Err(e),
    };
    let bundle = TrustBundle::parse_unverified(&signed.bundle_bytes()?)
        .context("the server's current trust bundle is malformed")?;
    Ok(Some(bundle.version))
}

pub async fn publish(args: PublishArgs) -> Result<()> {
    let mut session = Session::open(&args.server).await?;
    let info: ServerInfo = session.api.get("/.well-known/vgames.json").await?;
    // With --root, this also checks that the server advertises exactly that key.
    let pin = pin_for(&session, args.root.as_deref(), &info)?;

    let raw = std::fs::read(&args.signed)
        .with_context(|| format!("reading {}", args.signed.display()))?;
    let signed: SignedBundle = serde_json::from_slice(&raw).with_context(|| {
        format!(
            "{} is not signed bundle JSON (made by `vgames trust sign`)",
            args.signed.display()
        )
    })?;
    let bytes = signed.bundle_bytes()?;
    let (current_pin, last) = if args.new_identity {
        (pin.clone(), current_version_unverified(&session.api).await?)
    } else {
        let current = current_bundle(&session.api, &pin, info.server_id).await?;
        (
            current
                .as_ref()
                .map_or_else(|| pin.clone(), |c| c.pin.clone()),
            current.as_ref().map(|c| c.state.version()),
        )
    };
    let verified = verify_bundle(
        &bytes,
        &signed.signature,
        &current_pin,
        last,
        info.server_id,
    )
    .with_context(|| format!("{} does not verify for this server", args.signed.display()))?;
    if !verified.newer {
        bail!(
            "the server already has version {}; build the next one with `vgames trust build --previous`",
            verified.state.version()
        );
    }

    #[derive(Deserialize)]
    struct Stored {
        version: u64,
    }
    let stored: Stored = session.api.post("/v1/admin/trust/bundles", &signed).await?;
    session.update_pin(&verified.pin)?;
    println!(
        "Published trust bundle version {} to {} ({} publisher keys, {} revoked).",
        stored.version,
        info.name,
        verified.state.bundle().publishers.len(),
        verified.state.bundle().revoked.len()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// re-sign

#[derive(Deserialize)]
struct Page<T> {
    items: Vec<T>,
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct PackageItem {
    id: Uuid,
    #[serde(default)]
    title: String,
}

#[derive(Deserialize)]
struct Creator {
    #[serde(default)]
    username: String,
}

#[derive(Deserialize)]
struct VersionItem {
    id: Uuid,
    package_id: Uuid,
    platform: String,
    sequence: i64,
    version_label: String,
    state: String,
    #[serde(default)]
    is_current_release: Option<bool>,
    #[serde(default)]
    publisher_key_id: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    finalized_at: Option<OffsetDateTime>,
    created_by: Creator,
    /// The stored envelope, when the server exposes it on admin versions.
    #[serde(default)]
    signature: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Descriptor {
    version_id: Uuid,
    signature: serde_json::Value,
}

/// States whose signature launchers or the server still rely on.
const SIGNED_STATES: &[&str] = &["verifying", "ready", "published"];

async fn all_pages<T: serde::de::DeserializeOwned>(api: &Api, path: &str) -> Result<Vec<T>> {
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10_000 {
        let mut url = format!("{path}?limit=200");
        if let Some(c) = &cursor {
            url.push_str("&cursor=");
            url.push_str(&url::form_urlencoded::byte_serialize(c.as_bytes()).collect::<String>());
        }
        let page: Page<T> = api.get(&url).await?;
        items.extend(page.items);
        match page.next_cursor {
            Some(c) if !c.is_empty() => cursor = Some(c),
            _ => return Ok(items),
        }
    }
    bail!("{path}: too many pages")
}

pub(crate) fn envelope_from(value: &serde_json::Value) -> Result<Envelope> {
    let bytes = serde_json::to_vec(value)?;
    Envelope::parse(&bytes).map_err(|e| anyhow::anyhow!("invalid signature envelope: {e}"))
}

/// The old envelope of `v`: from the admin version if the server exposes it,
/// else from the release descriptor when `v` is the current release.
async fn old_envelope(api: &Api, v: &VersionItem) -> Result<Option<Envelope>> {
    if let Some(sig) = &v.signature {
        return envelope_from(sig).map(Some);
    }
    if v.is_current_release != Some(true) {
        return Ok(None);
    }
    let d: Descriptor = api
        .get(&format!(
            "/v1/packages/{}/releases/{}",
            v.package_id, v.platform
        ))
        .await?;
    if d.version_id != v.id {
        return Ok(None);
    }
    envelope_from(&d.signature).map(Some)
}

/// The old public key: from the current bundle, else from `--previous`. The key
/// id is a hash of the key, so any listing with a matching id is the key.
fn old_public_key(
    from: &KeyId,
    current: &TrustBundle,
    previous: Option<&Path>,
) -> Result<PublicKey> {
    let listed = |b: &TrustBundle| {
        b.publishers
            .iter()
            .find(|p| p.key_id == *from && p.public_key.key_id() == *from)
            .map(|p| p.public_key)
    };
    if let Some(pk) = listed(current) {
        return Ok(pk);
    }
    let Some(path) = previous else {
        bail!(
            "the current bundle no longer lists key {from}; pass --previous with a bundle that does"
        );
    };
    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let bytes = match serde_json::from_slice::<SignedBundle>(&raw) {
        Ok(s) => s.bundle_bytes()?,
        Err(_) => raw,
    };
    let bundle = TrustBundle::parse_unverified(&bytes)
        .with_context(|| format!("{} is not a trust bundle", path.display()))?;
    listed(&bundle).with_context(|| format!("{} does not list key {from} either", path.display()))
}

struct Plan<'a> {
    version: &'a VersionItem,
    package: &'a str,
    envelope: Envelope,
}

fn confirm(prompt: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("pass --yes to re-sign non-interactively");
    }
    eprint!("{prompt} [y/N] ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

pub async fn re_sign(args: ReSignArgs) -> Result<()> {
    let from: KeyId = args
        .from
        .parse()
        .map_err(|_| anyhow::anyhow!("--from must be a key id (32 lowercase hex)"))?;
    let before = args
        .finalized_before
        .as_deref()
        .map(|t| {
            OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339)
                .with_context(|| format!("--finalized-before {t:?} is not an RFC 3339 time"))
        })
        .transpose()?;
    let key_file = keys::load(&args.key)?;
    if key_file.kind != KeyKind::Publisher {
        bail!(
            "{} is a {} key, not a publisher key",
            args.key.display(),
            key_file.kind.as_str()
        );
    }
    let new_key = key_file.public_key;
    if new_key.key_id() == from {
        bail!("--key is the old key itself; issue a new publisher key first");
    }

    let mut session = Session::open(&args.server).await?;
    let info: ServerInfo = session.api.get("/.well-known/vgames.json").await?;
    let pin = pin_for(&session, args.root.as_deref(), &info)?;
    let Some(current) = current_bundle(&session.api, &pin, info.server_id).await? else {
        bail!("the server has no trust bundle yet");
    };
    session.update_pin(&current.pin)?;
    match current.state.key_status(&new_key.key_id()) {
        KeyStatus::Trusted(_) => {}
        KeyStatus::Revoked => bail!("your new key {} is revoked", new_key.key_id()),
        KeyStatus::Unknown => bail!(
            "your new key {} is not in the server's trust bundle (version {}); add it with \
             `vgames trust build` + `trust sign` + `trust publish` first",
            new_key.key_id(),
            current.state.version()
        ),
    }
    let old_key = old_public_key(&from, current.state.bundle(), args.previous.as_deref())?;

    // Every version signed by the old key.
    let packages: Vec<PackageItem> = all_pages(&session.api, "/v1/admin/packages").await?;
    let titles: BTreeMap<Uuid, String> = packages.iter().map(|p| (p.id, p.title.clone())).collect();
    let mut versions: Vec<VersionItem> = Vec::new();
    for p in &packages {
        let path = format!("/v1/admin/packages/{}/versions", p.id);
        versions.extend(
            all_pages::<VersionItem>(&session.api, &path)
                .await?
                .into_iter()
                .filter(|v| v.publisher_key_id.as_deref() == Some(from.to_hex().as_str()))
                .filter(|v| SIGNED_STATES.contains(&v.state.as_str())),
        );
    }

    let mut plans = Vec::new();
    let mut skipped: Vec<(&VersionItem, String)> = Vec::new();
    for v in &versions {
        if let Some(limit) = before
            && v.finalized_at.is_none_or(|f| f >= limit)
        {
            skipped.push((v, "finalized after --finalized-before".into()));
            continue;
        }
        let envelope = match old_envelope(&session.api, v).await {
            Ok(Some(e)) => e,
            Ok(None) => {
                skipped.push((
                    v,
                    "the server does not expose its signature (only current releases can be re-signed)".into(),
                ));
                continue;
            }
            Err(e) => {
                skipped.push((v, format!("{e:#}")));
                continue;
            }
        };
        // Never re-attest anything the old key did not sign.
        let genuine = envelope.context == SigContext::Manifest
            && envelope.key_id == from
            && envelope
                .verify_digest(&old_key, SigContext::Manifest, &envelope.payload_blake3)
                .is_ok();
        if !genuine {
            skipped.push((
                v,
                "its signature does not verify under the old key: NOT re-signed".into(),
            ));
            continue;
        }
        let package = titles.get(&v.package_id).map_or("?", String::as_str);
        plans.push(Plan {
            version: v,
            package,
            envelope,
        });
    }

    println!(
        "{} version(s) signed by {from} ({} to re-sign with {}):",
        versions.len(),
        plans.len(),
        new_key.key_id()
    );
    for p in &plans {
        let v = p.version;
        println!(
            "  re-sign  {}  {} {} #{} {:?}  {}  by {}  finalized {}",
            v.id,
            p.package,
            v.platform,
            v.sequence,
            v.version_label,
            v.state,
            v.created_by.username,
            v.finalized_at
                .map_or_else(|| "-".to_owned(), |t| t.to_string())
        );
    }
    for (v, why) in &skipped {
        println!("  skip     {}  {} #{}: {why}", v.id, v.platform, v.sequence);
    }
    if plans.is_empty() || args.dry_run {
        return Ok(());
    }
    if !args.yes && !confirm(&format!("Re-sign {} version(s)?", plans.len()))? {
        bail!("cancelled");
    }

    let passphrase = args.passphrase.read_existing("publisher key")?;
    let secret = key_file
        .decrypt(passphrase.as_bytes())
        .map_err(|e| anyhow::anyhow!("cannot unlock {}: {e}", args.key.display()))?;
    let mut failed = 0usize;
    for p in &plans {
        let new = Envelope::sign_digest(&secret, SigContext::Manifest, p.envelope.payload_blake3);
        new.verify_digest(&new_key, SigContext::Manifest, &p.envelope.payload_blake3)
            .map_err(|e| anyhow::anyhow!("the new signature does not verify: {e}"))?;
        let body = serde_json::json!({ "signature": new });
        let path = format!("/v1/admin/versions/{}/signature", p.version.id);
        match session.api.post::<_, serde_json::Value>(&path, &body).await {
            Ok(_) => println!("  re-signed {}", p.version.id),
            Err(e) => {
                failed += 1;
                eprintln!("  FAILED    {}: {e:#}", p.version.id);
            }
        }
    }
    drop(secret);
    if failed > 0 {
        bail!(
            "{failed} of {} version(s) could not be re-signed",
            plans.len()
        );
    }
    println!("Done: {} version(s) re-signed.", plans.len());
    Ok(())
}
