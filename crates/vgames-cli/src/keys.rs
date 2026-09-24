//! `vgames keys …`: root and publisher key ceremonies (01-security §3.1, §3.3).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde_json::json;
use vgames_core::Timestamp;
use vgames_core::keyfile::{KeyFile, KeyKind};
use vgames_core::sign::SecretKey;

use crate::secret::{PassphraseArgs, write_secret_file};

#[derive(Subcommand)]
pub enum KeysCmd {
    /// Create a server root key (run on an offline machine).
    #[command(after_help = "\
Examples:
  vgames keys init-root --out root.vgkey
  VGAMES_ROOT_PASS=… vgames keys init-root --out root.vgkey --passphrase-env VGAMES_ROOT_PASS

The root key signs trust bundles only. Keep two copies on separate offline media and
never copy it to the server: the server only needs the printed public key.")]
    InitRoot {
        /// Where to write the encrypted key file (created with mode 0600).
        #[arg(long, default_value = "root.vgkey")]
        out: PathBuf,
        /// A label to recognise the key later.
        #[arg(long, default_value = "server root key")]
        label: String,
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        passphrase: PassphraseArgs,
        /// Print machine-readable JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Create a publisher key for one admin on one machine.
    #[command(after_help = "\
Examples:
  vgames keys issue-publisher --label alice@workstation --out alice.vgkey

Send the printed public key (never the file) to the server owner, who adds it to the
next trust bundle with `vgames trust build` and `vgames trust sign`.")]
    IssuePublisher {
        /// Who and where, e.g. alice@workstation.
        #[arg(long)]
        label: String,
        /// Where to write the encrypted key file (created with mode 0600).
        #[arg(long, default_value = "publisher.vgkey")]
        out: PathBuf,
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        passphrase: PassphraseArgs,
        /// Print machine-readable JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Show a key file's public details (no passphrase needed).
    #[command(after_help = "\
Examples:
  vgames keys show root.vgkey
  vgames keys show alice.vgkey --json")]
    Show {
        file: PathBuf,
        /// Print machine-readable JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

pub fn run(cmd: KeysCmd) -> Result<()> {
    match cmd {
        KeysCmd::InitRoot {
            out,
            label,
            force,
            passphrase,
            json,
        } => create(KeyKind::Root, &out, &label, force, &passphrase, json),
        KeysCmd::IssuePublisher {
            label,
            out,
            force,
            passphrase,
            json,
        } => create(KeyKind::Publisher, &out, &label, force, &passphrase, json),
        KeysCmd::Show { file, json } => show(&file, json),
    }
}

pub fn now() -> Result<Timestamp> {
    Ok(Timestamp::from_unix(
        time::OffsetDateTime::now_utc().unix_timestamp(),
    )?)
}

/// Reads and parses a key file (public header only).
pub fn load(path: &Path) -> Result<KeyFile> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    KeyFile::parse(&bytes).with_context(|| format!("{} is not a vgames key file", path.display()))
}

/// Loads and decrypts a key file of the expected kind.
pub fn unlock(path: &Path, kind: KeyKind, passphrase: &PassphraseArgs) -> Result<SecretKey> {
    let file = load(path)?;
    if file.kind != kind {
        bail!(
            "{} is a {} key, a {} key is needed here",
            path.display(),
            file.kind.as_str(),
            kind.as_str()
        );
    }
    let what = format!("{} key {:?}", kind.as_str(), file.label);
    let pass = passphrase.read_existing(&what)?;
    Ok(file.decrypt(pass.as_bytes())?)
}

fn create(
    kind: KeyKind,
    out: &Path,
    label: &str,
    force: bool,
    passphrase: &PassphraseArgs,
    json: bool,
) -> Result<()> {
    if out.exists() && !force {
        bail!(
            "{} already exists (use --force to overwrite)",
            out.display()
        );
    }
    let pass = passphrase.read_new(&format!("{} key", kind.as_str()))?;
    let key = SecretKey::generate()?;
    let file = KeyFile::encrypt(&key, kind, label, now()?, pass.as_bytes())?;
    write_secret_file(out, &file.to_bytes(), force)?;
    // Prove the file round-trips before anyone relies on it.
    let reread = load(out)?;
    let back = reread.decrypt(pass.as_bytes())?;
    if back.public_key() != key.public_key() {
        bail!("the key file does not read back correctly");
    }
    drop(back);
    drop(key);
    let pk = file.public_key;
    if json {
        println!(
            "{}",
            json!({
                "file": out.display().to_string(),
                "kind": kind.as_str(),
                "label": label,
                "key_id": pk.key_id().to_hex(),
                "public_key": pk.to_base64(),
                "fingerprint": pk.fingerprint().to_string(),
            })
        );
        return Ok(());
    }
    println!(
        "Created {} key file {} (readable only by you).",
        kind.as_str(),
        out.display()
    );
    println!("  Key id:       {}", pk.key_id());
    println!("  Public key:   {}", pk.to_base64());
    println!("  Fingerprint:  {}", pk.fingerprint());
    println!();
    match kind {
        KeyKind::Root => {
            println!("Next steps:");
            println!(
                "  1. On the server, set VGAMES_ROOT_PUBLIC_KEY={}",
                pk.to_base64()
            );
            println!(
                "  2. Publish the fingerprint where your players can compare it (website, Discord)."
            );
            println!(
                "  3. Copy {} to two offline media. Never copy it to the server.",
                out.display()
            );
            println!("  4. Admins run `vgames keys issue-publisher`; add their public keys with");
            println!("     `vgames trust build` and `vgames trust sign`, then upload the bundle.");
        }
        KeyKind::Publisher => {
            println!("Send this to the server owner for the next trust bundle (trust.toml):");
            println!();
            println!("  [[publishers]]");
            println!("  public_key = \"{}\"", pk.to_base64());
            println!("  label = \"{}\"", label.replace('"', "'"));
            println!("  holder_user_id = \"<your vgames user id>\"");
            println!("  not_before = \"{}\"", file.created_at);
            println!("  not_after = \"<end of validity, e.g. two years>\"");
        }
    }
    Ok(())
}

fn show(path: &Path, json: bool) -> Result<()> {
    let f = load(path)?;
    let pk = f.public_key;
    if json {
        println!(
            "{}",
            json!({
                "kind": f.kind.as_str(),
                "label": f.label,
                "created_at": f.created_at.to_string(),
                "key_id": pk.key_id().to_hex(),
                "public_key": pk.to_base64(),
                "fingerprint": pk.fingerprint().to_string(),
            })
        );
    } else {
        println!("{} key {:?}", f.kind.as_str(), f.label);
        println!("  Created:      {}", f.created_at);
        println!("  Key id:       {}", pk.key_id());
        println!("  Public key:   {}", pk.to_base64());
        println!("  Fingerprint:  {}", pk.fingerprint());
    }
    Ok(())
}
