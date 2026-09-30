//! `vgames trust …`: build, sign and verify root-signed trust bundles
//! (01-security §3.2). Signing happens offline with the root key file.

use std::collections::BTreeMap;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde::Deserialize;
use vgames_core::keyfile::KeyKind;
use vgames_core::sign::{KeyId, PublicKey};
use vgames_core::trust::{
    NextRoot, PublisherKey, Revocation, RootPin, SignedBundle, TrustBundle, sign_bundle,
    verify_bundle,
};
use vgames_core::{Fingerprint, Timestamp};

use crate::keys;
use crate::secret::PassphraseArgs;

#[derive(Subcommand)]
pub enum TrustCmd {
    /// Build the next trust bundle from a spec file (unsigned; sign it offline).
    #[command(after_help = "\
Examples:
  vgames trust build --spec trust.toml --root root.vgkey --out bundle.json
  vgames trust build --spec trust.toml --root root.vgkey --previous current.signed.json --out bundle.json

trust.toml:
  server_id = \"0192…\"                    # VGAMES_SERVER_ID of the server
  expires_at = \"2027-09-24T00:00:00Z\"    # optional
  [[publishers]]
  public_key = \"…base64…\"                # from `vgames keys issue-publisher`
  label = \"alice@workstation\"
  holder_user_id = \"0192…\"               # the admin's vgames user id
  not_before = \"2026-09-24T00:00:00Z\"
  not_after = \"2028-09-24T00:00:00Z\"
  [[revoked]]
  key_id = \"…32 hex…\"
  reason = \"laptop stolen\"               # revoked_at defaults to now
  [next_root]                             # optional: announce a root rotation
  public_key = \"…base64…\"

The version is the previous bundle's + 1, and every revocation of the previous bundle
is carried over: a key, once revoked, stays revoked.")]
    Build {
        /// The trust spec (TOML, format below).
        #[arg(long)]
        spec: PathBuf,
        /// The root key file (only its public part is read) or a base64 root public key.
        #[arg(long)]
        root: String,
        /// The current bundle (signed `{bundle, signature}` JSON, or the bundle bytes).
        #[arg(long)]
        previous: Option<PathBuf>,
        /// Where to write the unsigned bundle bytes.
        #[arg(long, default_value = "bundle.json")]
        out: PathBuf,
        /// Overwrite --out if it exists.
        #[arg(long)]
        force: bool,
    },
    /// Sign bundle bytes with the root key (offline). Writes `{bundle, signature}` JSON.
    #[command(after_help = "\
Examples:
  vgames trust sign --bundle bundle.json --root root.vgkey --out bundle.signed.json
  vgames trust sign --bundle bundle.json --root root.vgkey --yes --passphrase-env ROOT_PASS

Upload the output in the admin UI (Trust) or with `vgames trust publish`.")]
    Sign {
        /// The unsigned bundle from `vgames trust build`.
        #[arg(long)]
        bundle: PathBuf,
        /// The root key file.
        #[arg(long)]
        root: PathBuf,
        /// Where to write the signed `{bundle, signature}` JSON.
        #[arg(long, default_value = "bundle.signed.json")]
        out: PathBuf,
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
        /// Overwrite --out if it exists.
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        passphrase: PassphraseArgs,
    },
    /// Verify a signed bundle against a root public key.
    #[command(after_help = "\
Examples:
  vgames trust verify --signed bundle.signed.json --root root.vgkey
  vgames trust verify --signed bundle.signed.json --root <base64> --fingerprint VG1-… --server-id 0192…")]
    Verify {
        /// The signed `{bundle, signature}` JSON to check.
        #[arg(long)]
        signed: PathBuf,
        /// The root key file (public part) or a base64 root public key.
        #[arg(long)]
        root: String,
        /// Also accept a signature by this announced next root (base64).
        #[arg(long)]
        next_root: Option<String>,
        /// Require the root key to have this fingerprint.
        #[arg(long)]
        fingerprint: Option<String>,
        /// Require this server id.
        #[arg(long)]
        server_id: Option<String>,
        /// Refuse a bundle older than this version.
        #[arg(long)]
        last_version: Option<u64>,
    },
    /// Upload a signed bundle to the server (owner; verified locally first).
    Publish(crate::trust_server::PublishArgs),
    /// Re-sign every version signed by an old publisher key with your new one.
    ReSign(crate::trust_server::ReSignArgs),
}

pub fn run(cmd: TrustCmd) -> Result<()> {
    match cmd {
        TrustCmd::Build {
            spec,
            root,
            previous,
            out,
            force,
        } => build(&spec, &root, previous.as_deref(), &out, force),
        TrustCmd::Sign {
            bundle,
            root,
            out,
            yes,
            force,
            passphrase,
        } => sign(&bundle, &root, &out, yes, force, &passphrase),
        TrustCmd::Verify {
            signed,
            root,
            next_root,
            fingerprint,
            server_id,
            last_version,
        } => verify(
            &signed,
            &root,
            next_root.as_deref(),
            fingerprint.as_deref(),
            server_id.as_deref(),
            last_version,
        ),
        TrustCmd::Publish(args) => crate::block_on(crate::trust_server::publish(args)),
        TrustCmd::ReSign(args) => crate::block_on(crate::trust_server::re_sign(args)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    server_id: String,
    #[serde(default)]
    issued_at: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    publishers: Vec<SpecPublisher>,
    #[serde(default)]
    revoked: Vec<SpecRevocation>,
    #[serde(default)]
    next_root: Option<SpecNextRoot>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecPublisher {
    public_key: String,
    label: String,
    holder_user_id: String,
    not_before: String,
    not_after: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecRevocation {
    key_id: String,
    #[serde(default)]
    revoked_at: Option<String>,
    #[serde(default)]
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecNextRoot {
    public_key: String,
}

/// A root public key from a key file path or base64.
pub fn root_key(arg: &str) -> Result<PublicKey> {
    let path = Path::new(arg);
    if path.exists() {
        let f = keys::load(path)?;
        if f.kind != KeyKind::Root {
            bail!("{arg} is a {} key, not a root key", f.kind.as_str());
        }
        return Ok(f.public_key);
    }
    PublicKey::from_base64(arg)
        .with_context(|| format!("{arg:?} is neither a key file nor a base64 public key"))
}

fn ts(field: &str, s: &str) -> Result<Timestamp> {
    s.parse().with_context(|| {
        format!("{field}: {s:?} is not an RFC 3339 UTC time like 2026-09-24T10:00:00Z")
    })
}

/// Reads a bundle file: signed `{bundle, signature}` JSON or raw bundle bytes.
fn read_bundle(path: &Path) -> Result<(Vec<u8>, Option<SignedBundle>)> {
    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if let Ok(signed) = serde_json::from_slice::<SignedBundle>(&raw) {
        let bytes = signed.bundle_bytes()?;
        return Ok((bytes, Some(signed)));
    }
    Ok((raw, None))
}

fn build(
    spec_path: &Path,
    root: &str,
    previous: Option<&Path>,
    out: &Path,
    force: bool,
) -> Result<()> {
    if out.exists() && !force {
        bail!(
            "{} already exists (use --force to overwrite)",
            out.display()
        );
    }
    let text = std::fs::read_to_string(spec_path)
        .with_context(|| format!("reading {}", spec_path.display()))?;
    let spec: Spec =
        toml::from_str(&text).with_context(|| format!("parsing {}", spec_path.display()))?;
    let root = root_key(root)?;
    let server_id = vgames_core::trust::parse_server_id(&spec.server_id)
        .context("server_id must be a lowercase UUID")?;
    let now = keys::now()?;

    let prev = match previous {
        Some(p) => {
            let (bytes, _) = read_bundle(p)?;
            let b = TrustBundle::parse_unverified(&bytes)
                .with_context(|| format!("{} is not a valid trust bundle", p.display()))?;
            if b.server_id != server_id {
                bail!(
                    "the previous bundle is for server {}, the spec for {server_id}",
                    b.server_id
                );
            }
            Some(b)
        }
        None => None,
    };

    // Revocations are permanent: carry every previous one over.
    let mut revoked: BTreeMap<KeyId, Revocation> = BTreeMap::new();
    for r in prev.iter().flat_map(|b| b.revoked.iter()) {
        revoked.insert(r.key_id, r.clone());
    }
    for r in &spec.revoked {
        let key_id: KeyId = r
            .key_id
            .parse()
            .with_context(|| format!("revoked key_id {:?}", r.key_id))?;
        let revoked_at = match &r.revoked_at {
            Some(s) => ts("revoked_at", s)?,
            None => now,
        };
        revoked.entry(key_id).or_insert(Revocation {
            key_id,
            revoked_at,
            reason: r.reason.clone(),
        });
    }

    let mut publishers = Vec::new();
    for p in &spec.publishers {
        let public_key = PublicKey::from_base64(&p.public_key)
            .with_context(|| format!("publisher {:?}: public_key", p.label))?;
        let key_id = public_key.key_id();
        if revoked.contains_key(&key_id) {
            bail!(
                "publisher {:?} ({key_id}) is revoked; revoked keys stay revoked, issue a new key",
                p.label
            );
        }
        publishers.push(PublisherKey {
            key_id,
            public_key,
            holder_user_id: vgames_core::trust::parse_server_id(&p.holder_user_id).with_context(
                || {
                    format!(
                        "publisher {:?}: holder_user_id must be a lowercase UUID",
                        p.label
                    )
                },
            )?,
            label: p.label.clone(),
            not_before: ts("not_before", &p.not_before)?,
            not_after: ts("not_after", &p.not_after)?,
        });
    }

    let next_root = match &spec.next_root {
        Some(n) => {
            let pk = PublicKey::from_base64(&n.public_key).context("next_root.public_key")?;
            Some(NextRoot {
                key_id: pk.key_id(),
                public_key: pk,
            })
        }
        None => None,
    };

    let bundle = TrustBundle {
        format: vgames_core::trust::FORMAT.into(),
        server_id,
        version: prev.as_ref().map_or(1, |b| b.version + 1),
        issued_at: match &spec.issued_at {
            Some(s) => ts("issued_at", s)?,
            None => now,
        },
        expires_at: spec
            .expires_at
            .as_deref()
            .map(|s| ts("expires_at", s))
            .transpose()?,
        root_key_id: root.key_id(),
        publishers,
        revoked: revoked.into_values().collect(),
        next_root,
    };
    let bytes = bundle.to_bytes();
    // The same structural rules the server and launchers apply.
    TrustBundle::parse_unverified(&bytes).context("the resulting bundle is invalid")?;
    std::fs::write(out, &bytes).with_context(|| format!("writing {}", out.display()))?;

    print_summary(&bundle, prev.as_ref());
    println!();
    println!(
        "Wrote {} (unsigned). Next: vgames trust sign --bundle {} --root <root.vgkey>",
        out.display(),
        out.display()
    );
    Ok(())
}

fn print_summary(b: &TrustBundle, prev: Option<&TrustBundle>) {
    println!("Trust bundle v{} for server {}", b.version, b.server_id);
    println!("  Issued:     {}", b.issued_at);
    match b.expires_at {
        Some(e) => {
            println!("  Expires:    {e} (after this, installs and updates wait for a new bundle)")
        }
        None => println!("  Expires:    never"),
    }
    println!("  Root key:   {}", b.root_key_id);
    println!("  Publishers ({}):", b.publishers.len());
    for p in &b.publishers {
        println!(
            "    {:<28} key {}  holder {}  valid {} → {}",
            p.label, p.key_id, p.holder_user_id, p.not_before, p.not_after
        );
    }
    println!("  Revoked ({}):", b.revoked.len());
    for r in &b.revoked {
        println!("    key {}  since {}  {}", r.key_id, r.revoked_at, r.reason);
    }
    match &b.next_root {
        Some(n) => println!(
            "  Next root:  {} ({}): launchers will accept bundles signed by it and re-pin",
            n.key_id,
            n.public_key.fingerprint()
        ),
        None => println!("  Next root:  none"),
    }
    if let Some(prev) = prev {
        let had = |id: &KeyId| prev.publishers.iter().any(|p| &p.key_id == id);
        let has = |id: &KeyId| b.publishers.iter().any(|p| &p.key_id == id);
        for p in b.publishers.iter().filter(|p| !had(&p.key_id)) {
            println!("  + adds publisher {} ({})", p.label, p.key_id);
        }
        for p in prev.publishers.iter().filter(|p| !has(&p.key_id)) {
            let revoked = b.revoked.iter().any(|r| r.key_id == p.key_id);
            if revoked {
                println!(
                    "  ✗ revokes {} ({}): packages it signed stop launching until re-signed",
                    p.label, p.key_id
                );
            } else {
                println!(
                    "  ! drops publisher {} ({}) without revoking it: packages it signed stop verifying until re-signed",
                    p.label, p.key_id
                );
            }
        }
    }
}

fn confirm(question: &str) -> Result<bool> {
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

fn sign(
    bundle_path: &Path,
    root_path: &Path,
    out: &Path,
    yes: bool,
    force: bool,
    passphrase: &PassphraseArgs,
) -> Result<()> {
    if out.exists() && !force {
        bail!(
            "{} already exists (use --force to overwrite)",
            out.display()
        );
    }
    let (bytes, signed) = read_bundle(bundle_path)?;
    if signed.is_some() {
        bail!("{} is already signed", bundle_path.display());
    }
    let bundle = TrustBundle::parse_unverified(&bytes)
        .with_context(|| format!("{} is not a valid trust bundle", bundle_path.display()))?;
    let root_file = keys::load(root_path)?;
    if root_file.kind != KeyKind::Root {
        bail!("{} is not a root key", root_path.display());
    }
    if root_file.key_id != bundle.root_key_id {
        bail!(
            "the bundle names root key {}, but {} is key {}",
            bundle.root_key_id,
            root_path.display(),
            root_file.key_id
        );
    }
    print_summary(&bundle, None);
    println!();
    if !yes && !confirm("Sign this bundle with the root key?")? {
        bail!("not signed");
    }
    let key = keys::unlock(root_path, KeyKind::Root, passphrase)?;
    let signature = sign_bundle(&key, &bytes);
    drop(key);
    // Self-check with the same function launchers use.
    verify_bundle(
        &bytes,
        &signature,
        &RootPin::new(root_file.public_key),
        None,
        bundle.server_id,
    )
    .context("the new signature does not verify")?;
    let mut json = serde_json::to_vec_pretty(&SignedBundle::new(&bytes, signature))?;
    json.push(b'\n');
    std::fs::write(out, json).with_context(|| format!("writing {}", out.display()))?;
    println!(
        "Signed. Wrote {}: upload it in the admin UI (Trust) or with `vgames trust publish`.",
        out.display()
    );
    Ok(())
}

fn verify(
    signed_path: &Path,
    root: &str,
    next_root: Option<&str>,
    fingerprint: Option<&str>,
    server_id: Option<&str>,
    last_version: Option<u64>,
) -> Result<()> {
    let (bytes, signed) = read_bundle(signed_path)?;
    let Some(signed) = signed else {
        bail!(
            "{} is not a signed bundle ({{\"bundle\", \"signature\"}})",
            signed_path.display()
        );
    };
    let root = root_key(root)?;
    if let Some(fp) = fingerprint {
        let expected: Fingerprint = fp.parse().context("--fingerprint")?;
        if root.fingerprint() != expected {
            bail!(
                "the root key's fingerprint is {}, not {expected}",
                root.fingerprint()
            );
        }
    }
    let pin = RootPin {
        root,
        next_root: next_root
            .map(PublicKey::from_base64)
            .transpose()
            .context("--next-root")?,
    };
    let unverified = TrustBundle::parse_unverified(&bytes)?;
    let server = match server_id {
        Some(s) => vgames_core::trust::parse_server_id(s).context("--server-id")?,
        None => {
            println!("note: --server-id not given; checking against the bundle's own server id");
            unverified.server_id
        }
    };
    let v = verify_bundle(&bytes, &signed.signature, &pin, last_version, server)?;
    print_summary(v.state.bundle(), None);
    println!();
    println!(
        "Signature valid: signed by root {} ({}){}",
        v.pin.root.key_id(),
        v.pin.root.fingerprint(),
        if v.rotated {
            ", completing a root rotation"
        } else {
            ""
        }
    );
    Ok(())
}
