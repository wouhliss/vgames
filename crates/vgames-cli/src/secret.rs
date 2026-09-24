//! Passphrases and secret files for the key ceremonies.
//!
//! Interactive by default (the passphrase is read from the terminal, never
//! echoed). For CI and scripts: `--passphrase-env VAR` or `--passphrase-file
//! PATH`. Nothing here ever prints or logs a passphrase.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use zeroize::Zeroizing;

/// Where a passphrase comes from.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct PassphraseArgs {
    /// Read the passphrase from this environment variable (non-interactive).
    #[arg(long, value_name = "VAR", conflicts_with = "passphrase_file")]
    pub passphrase_env: Option<String>,
    /// Read the passphrase from the first line of this file (non-interactive).
    #[arg(long, value_name = "PATH")]
    pub passphrase_file: Option<PathBuf>,
}

impl PassphraseArgs {
    fn non_interactive(&self) -> Result<Option<Zeroizing<String>>> {
        if let Some(var) = &self.passphrase_env {
            let v = std::env::var(var)
                .with_context(|| format!("environment variable {var} is not set"))?;
            return Ok(Some(Zeroizing::new(v)));
        }
        if let Some(path) = &self.passphrase_file {
            let v = Zeroizing::new(
                std::fs::read_to_string(path)
                    .with_context(|| format!("reading {}", path.display()))?,
            );
            let line = v.lines().next().unwrap_or("").to_owned();
            return Ok(Some(Zeroizing::new(line)));
        }
        Ok(None)
    }

    /// A new passphrase: asked twice when interactive, always strength-checked.
    pub fn read_new(&self, what: &str) -> Result<Zeroizing<String>> {
        let pass = match self.non_interactive()? {
            Some(p) => p,
            None => {
                let first = Zeroizing::new(
                    rpassword::prompt_password(format!("New passphrase for the {what}: "))
                        .context("reading the passphrase (use --passphrase-env in scripts)")?,
                );
                let second = Zeroizing::new(
                    rpassword::prompt_password("Repeat the passphrase: ")
                        .context("reading the passphrase")?,
                );
                if *first != *second {
                    bail!("the two passphrases differ");
                }
                first
            }
        };
        if let Err(why) = check_strength(&pass) {
            bail!("passphrase too weak: {why}");
        }
        Ok(pass)
    }

    /// The passphrase of an existing key file.
    pub fn read_existing(&self, what: &str) -> Result<Zeroizing<String>> {
        match self.non_interactive()? {
            Some(p) => Ok(p),
            None => Ok(Zeroizing::new(
                rpassword::prompt_password(format!("Passphrase for the {what}: "))
                    .context("reading the passphrase (use --passphrase-env in scripts)")?,
            )),
        }
    }
}

const COMMON: &[&str] = &[
    "password",
    "passphrase",
    "vgames",
    "123456",
    "qwerty",
    "azerty",
    "letmein",
    "admin",
    "secret",
    "changeme",
    "iloveyou",
    "welcome",
    "dragon",
    "monkey",
];

/// A conservative estimate: ≥ 12 characters, ≥ 6 distinct characters,
/// ≥ 64 bits by character-class pool size, and not built around a common word.
pub fn check_strength(p: &str) -> Result<(), String> {
    let n = p.chars().count();
    if n < 12 {
        return Err("use at least 12 characters (four or more random words work well)".into());
    }
    let mut distinct: Vec<char> = p.chars().collect();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.len() < 6 {
        return Err("too many repeated characters".into());
    }
    let mut pool = 0u32;
    if p.chars().any(|c| c.is_ascii_lowercase()) {
        pool += 26;
    }
    if p.chars().any(|c| c.is_ascii_uppercase()) {
        pool += 26;
    }
    if p.chars().any(|c| c.is_ascii_digit()) {
        pool += 10;
    }
    if p.chars().any(|c| c.is_ascii_punctuation() || c == ' ') {
        pool += 33;
    }
    if !p.is_ascii() {
        pool += 100;
    }
    let bits = n as f64 * f64::from(pool.max(2)).log2();
    if bits < 64.0 {
        return Err(format!(
            "about {bits:.0} bits of entropy, 64 needed: make it longer"
        ));
    }
    let lower = p.to_lowercase();
    if let Some(word) = COMMON.iter().find(|w| lower.contains(*w))
        && lower.len() < word.len() + 12
    {
        return Err(format!("built around the common word {word:?}"));
    }
    Ok(())
}

/// Writes a secret file readable only by the current user (mode 0600 on Unix;
/// on Windows the file inherits the user profile's ACL). Refuses to overwrite
/// unless `force`.
pub fn write_secret_file(path: &Path, bytes: &[u8], force: bool) -> Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true);
    if force {
        opts.create(true).truncate(true);
    } else {
        opts.create_new(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).with_context(|| {
        if path.exists() && !force {
            format!(
                "{} already exists (use --force to overwrite)",
                path.display()
            )
        } else {
            format!("creating {}", path.display())
        }
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // An existing file keeps its old mode when truncated.
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strength() {
        for weak in [
            "short",
            "aaaaaaaaaaaaaaaaaaaa",
            "abcabcabcabc",
            "password1234",
            "vgamesvgames1",
            "123456789012",
        ] {
            assert!(check_strength(weak).is_err(), "{weak}");
        }
        for ok in [
            "correct horse battery staple",
            "Tr0ub4dor&3-plus-more",
            "mon mot de passe très long",
            "passwordisnotenough butthisislongerstill",
        ] {
            assert!(check_strength(ok).is_ok(), "{ok}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn secret_files_are_private_and_not_overwritten() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("k.vgkey");
        write_secret_file(&p, b"one", false).unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(write_secret_file(&p, b"two", false).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"one");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_secret_file(&p, b"two", true).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
