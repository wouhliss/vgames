//! Launcher-update artifacts for the release workflow (08-release §2, A5-T07).
//!
//! - `sign`: signs each updater artifact with the Tauri updater key (minisign),
//!   writing `<artifact>.sig` in Tauri's format with `version:<V>` in the
//!   trusted comment. The launcher sets `requireSignedVersion`, so a signature
//!   without the version, or for another version, is refused: an older build
//!   cannot be replayed under a newer version number.
//! - `manifest`: writes `latest.json` after verifying every artifact against
//!   the public key compiled into the launcher (`tauri.conf.json`).
//! - `verify`: re-checks a finished `latest.json` + artifacts directory (the
//!   final gate of the release job, and the dry-run evidence).

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

/// Largest artifact read into memory for signing or verification.
const MAX_ARTIFACT: u64 = 1024 * 1024 * 1024;
/// Updater target keys the launcher's plugin looks up (`{os}-{arch}[-{installer}]`).
const TARGET_OS: &[&str] = &["windows", "darwin", "linux"];
const TARGET_ARCH: &[&str] = &["x86_64", "aarch64", "i686", "armv7"];
const TARGET_INSTALLER: &[&str] = &["nsis", "msi", "app", "appimage", "deb", "rpm"];

fn read_artifact(path: &Path) -> Result<Vec<u8>> {
    let len = std::fs::metadata(path)
        .with_context(|| format!("reading {}", path.display()))?
        .len();
    if len > MAX_ARTIFACT {
        bail!("{} is larger than 1 GiB", path.display());
    }
    std::fs::read(path).with_context(|| format!("reading {}", path.display()))
}

fn file_name(path: &Path) -> Result<&str> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("{} has no UTF-8 file name", path.display()))?;
    if name.contains(['\t', '\n', '\r']) {
        bail!("{name:?}: file names with tabs or newlines cannot be signed");
    }
    Ok(name)
}

fn semver(version: &str) -> Result<semver::Version> {
    let v = semver::Version::parse(version)
        .with_context(|| format!("{version:?} is not a semver version"))?;
    if !v.build.is_empty() {
        bail!("{version}: build metadata is ignored by version comparisons; do not use it");
    }
    Ok(v)
}

/// Decodes a key or signature as Tauri stores it: base64 of the minisign text,
/// or the text itself.
fn minisign_text(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.starts_with("untrusted comment:") {
        return Ok(trimmed.to_owned());
    }
    let bytes = BASE64
        .decode(trimmed)
        .context("not base64 and not minisign text")?;
    String::from_utf8(bytes).context("the decoded value is not text")
}

/// The secret key from `TAURI_SIGNING_PRIVATE_KEY` (content or file path).
fn load_secret_key(value: &str, password: Option<&str>) -> Result<minisign::SecretKey> {
    let text = match Path::new(value.trim()).is_file() {
        true => std::fs::read_to_string(value.trim()).context("reading the signing key file")?,
        false => value.to_owned(),
    };
    let text = minisign_text(&text).context("TAURI_SIGNING_PRIVATE_KEY")?;
    let boxed = || minisign::SecretKeyBox::from_string(&text).context("not a minisign secret key");
    // Tauri keys are encrypted, with an empty password when none was chosen.
    match boxed()?.into_unencrypted_secret_key() {
        Ok(key) => Ok(key),
        Err(_) => boxed()?
            .into_secret_key(Some(password.unwrap_or_default().to_owned()))
            .context("cannot decrypt the signing key (wrong TAURI_SIGNING_PRIVATE_KEY_PASSWORD?)"),
    }
}

/// The public key from `plugins.updater.pubkey` of a `tauri.conf.json`, or a
/// value given directly (base64 of the minisign public key file, like the config).
pub fn load_public_key(pubkey: Option<&str>, tauri_conf: &Path) -> Result<minisign::PublicKey> {
    let value = match pubkey {
        Some(v) => v.to_owned(),
        None => {
            let conf: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(tauri_conf)
                    .with_context(|| format!("reading {}", tauri_conf.display()))?,
            )
            .with_context(|| format!("parsing {}", tauri_conf.display()))?;
            conf.pointer("/plugins/updater/pubkey")
                .and_then(|v| v.as_str())
                .with_context(|| format!("{} has no plugins.updater.pubkey", tauri_conf.display()))?
                .to_owned()
        }
    };
    if value.starts_with("REPLACE_WITH") {
        bail!(
            "the updater public key is still a placeholder in {}",
            tauri_conf.display()
        );
    }
    let text = minisign_text(&value).context("updater public key")?;
    minisign::PublicKeyBox::from_string(&text)
        .and_then(|b| b.into_public_key())
        .context("not a minisign public key")
}

fn trusted_comment(name: &str, version: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("timestamp:{now}\tfile:{name}\tversion:{version}")
}

/// Signs `data` as Tauri does; returns the `.sig` content (base64 of the text).
fn sign_bytes(key: &minisign::SecretKey, data: &[u8], name: &str, version: &str) -> Result<String> {
    let signature = minisign::sign(
        None,
        key,
        Cursor::new(data),
        Some(&trusted_comment(name, version)),
        Some("signature from tauri secret key"),
    )
    .context("signing")?;
    Ok(BASE64.encode(signature.to_string()))
}

/// Verifies a `.sig` (Tauri format) over `data`; returns the signed version.
pub fn verify_bytes(pk: &minisign::PublicKey, data: &[u8], sig: &str) -> Result<String> {
    let text = minisign_text(sig).context("signature")?;
    let signature =
        minisign::SignatureBox::from_string(&text).context("not a minisign signature")?;
    minisign::verify(pk, &signature, Cursor::new(data), true, false, false)
        .context("the signature does not verify with the updater public key")?;
    // Only now is the trusted comment authenticated.
    let comment = signature.trusted_comment().context("trusted comment")?;
    comment
        .split('\t')
        .find_map(|f| f.strip_prefix("version:"))
        .map(str::to_owned)
        .context("the signature does not name a version")
}

pub fn sign(version: &str, key: &str, password: Option<&str>, files: &[PathBuf]) -> Result<()> {
    semver(version)?;
    let key = load_secret_key(key, password)?;
    for path in files {
        let name = file_name(path)?;
        let data = read_artifact(path)?;
        let sig = sign_bytes(&key, &data, name, version)?;
        let out = PathBuf::from(format!("{}.sig", path.display()));
        std::fs::write(&out, &sig).with_context(|| format!("writing {}", out.display()))?;
        println!("signed {name} for version {version}");
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Latest {
    pub version: String,
    pub notes: String,
    pub pub_date: String,
    pub platforms: BTreeMap<String, Platform>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Platform {
    pub signature: String,
    pub url: String,
}

fn check_target(target: &str) -> Result<()> {
    let parts: Vec<&str> = target.split('-').collect();
    let ok = match parts.as_slice() {
        [os, arch] => TARGET_OS.contains(os) && TARGET_ARCH.contains(arch),
        [os, arch, installer] => {
            TARGET_OS.contains(os)
                && TARGET_ARCH.contains(arch)
                && TARGET_INSTALLER.contains(installer)
        }
        _ => false,
    };
    if !ok {
        bail!("{target:?} is not an updater target like windows-x86_64 or linux-x86_64-appimage");
    }
    Ok(())
}

/// `target=path` pairs.
pub fn parse_artifacts(pairs: &[String]) -> Result<Vec<(String, PathBuf)>> {
    pairs
        .iter()
        .map(|p| {
            let (t, f) = p
                .split_once('=')
                .with_context(|| format!("{p:?}: expected TARGET=FILE"))?;
            check_target(t)?;
            Ok((t.to_owned(), PathBuf::from(f)))
        })
        .collect()
}

pub struct ManifestArgs<'a> {
    pub version: &'a str,
    pub notes: &'a Path,
    pub pub_date: &'a str,
    pub url_base: &'a str,
    pub pubkey: &'a minisign::PublicKey,
    pub artifacts: &'a [(String, PathBuf)],
}

pub fn manifest(args: &ManifestArgs<'_>) -> Result<Latest> {
    let v = semver(args.version)?;
    let url_base = url_base(args.url_base)?;
    time::OffsetDateTime::parse(
        args.pub_date,
        &time::format_description::well_known::Rfc3339,
    )
    .with_context(|| format!("--pub-date {:?} is not RFC 3339", args.pub_date))?;
    let notes = std::fs::read_to_string(args.notes)
        .with_context(|| format!("reading {}", args.notes.display()))?;
    if notes.trim().is_empty() {
        bail!("{} is empty", args.notes.display());
    }
    if args.artifacts.is_empty() {
        bail!("no artifacts");
    }
    let mut platforms = BTreeMap::new();
    for (target, path) in args.artifacts {
        let name = file_name(path)?;
        let data = read_artifact(path)?;
        let sig_path = PathBuf::from(format!("{}.sig", path.display()));
        let sig = std::fs::read_to_string(&sig_path)
            .with_context(|| format!("reading {}", sig_path.display()))?;
        let signed =
            verify_bytes(args.pubkey, &data, &sig).with_context(|| format!("{name} ({target})"))?;
        if semver(&signed).ok() != Some(v.clone()) {
            bail!(
                "{name} is signed for version {signed}, not {}",
                args.version
            );
        }
        let url = format!("{url_base}{}", encode_segment(name));
        if platforms
            .insert(
                target.clone(),
                Platform {
                    signature: sig.trim().to_owned(),
                    url,
                },
            )
            .is_some()
        {
            bail!("{target} is listed twice");
        }
    }
    Ok(Latest {
        version: args.version.to_owned(),
        notes: notes.trim_end().to_owned(),
        pub_date: args.pub_date.to_owned(),
        platforms,
    })
}

fn url_base(s: &str) -> Result<String> {
    let base = if s.ends_with('/') {
        s.to_owned()
    } else {
        format!("{s}/")
    };
    let url = url_parse_https(&base)?;
    Ok(url)
}

fn url_parse_https(s: &str) -> Result<String> {
    if !s.starts_with("https://") || s.contains(['?', '#', ' ']) {
        bail!("{s:?}: the download base must be a plain https:// URL");
    }
    Ok(s.to_owned())
}

fn encode_segment(name: &str) -> String {
    name.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Checks `latest.json` against the artifacts in `dir` (named as in the URLs).
pub fn verify(latest: &Path, dir: &Path, pubkey: &minisign::PublicKey) -> Result<usize> {
    let doc: Latest = serde_json::from_str(
        &std::fs::read_to_string(latest)
            .with_context(|| format!("reading {}", latest.display()))?,
    )
    .with_context(|| format!("{} is not a valid latest.json", latest.display()))?;
    let v = semver(&doc.version)?;
    if doc.platforms.is_empty() {
        bail!("latest.json lists no platform");
    }
    for (target, p) in &doc.platforms {
        check_target(target)?;
        url_parse_https(&p.url)?;
        let name = p.url.rsplit('/').next().unwrap_or_default();
        let name = percent_decode(name)?;
        if name.is_empty() || name.contains(['/', '\\']) || name == ".." {
            bail!("{target}: bad artifact name in {}", p.url);
        }
        let data = read_artifact(&dir.join(&name))?;
        let signed = verify_bytes(pubkey, &data, &p.signature)
            .with_context(|| format!("{target}: {name}"))?;
        if semver(&signed).ok() != Some(v.clone()) {
            bail!(
                "{target}: {name} is signed for {signed}, latest.json announces {}",
                doc.version
            );
        }
        println!("ok  {target:<24} {name} (signed for {signed})");
    }
    Ok(doc.platforms.len())
}

fn percent_decode(s: &str) -> Result<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hex = s.get(i + 1..i + 3).context("bad percent escape")?;
            out.push(u8::from_str_radix(hex, 16).context("bad percent escape")?);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).context("artifact name is not UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Keys {
        secret_text: String,
        pubkey_config: String,
        pk: minisign::PublicKey,
    }

    fn keys(password: Option<&str>) -> Keys {
        let pair = match password {
            Some(p) => minisign::KeyPair::generate_encrypted_keypair(Some(p.to_owned())).unwrap(),
            None => minisign::KeyPair::generate_unencrypted_keypair().unwrap(),
        };
        Keys {
            secret_text: pair.sk.to_box(None).unwrap().to_string(),
            pubkey_config: BASE64.encode(pair.pk.to_box().unwrap().to_string()),
            pk: pair.pk,
        }
    }

    #[test]
    fn signatures_carry_the_version_and_verify() {
        let k = keys(None);
        let key = load_secret_key(&BASE64.encode(&k.secret_text), None).unwrap();
        let sig = sign_bytes(&key, b"artifact", "vgames.AppImage", "0.4.0").unwrap();
        assert_eq!(verify_bytes(&k.pk, b"artifact", &sig).unwrap(), "0.4.0");
        assert!(verify_bytes(&k.pk, b"tampered", &sig).is_err());
        assert!(
            verify_bytes(&keys(None).pk, b"artifact", &sig).is_err(),
            "another key"
        );
        let text = minisign_text(&sig).unwrap();
        assert!(
            text.contains("\tfile:vgames.AppImage\tversion:0.4.0"),
            "{text}"
        );
    }

    #[test]
    fn a_signature_without_a_version_is_refused() {
        let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let sig = minisign::sign(
            None,
            &pair.sk,
            Cursor::new(b"a"),
            Some("timestamp:1\tfile:a"),
            None,
        )
        .unwrap()
        .to_string();
        let err = verify_bytes(&pair.pk, b"a", &BASE64.encode(sig)).unwrap_err();
        assert!(
            err.to_string().contains("does not name a version"),
            "{err:#}"
        );
    }

    #[test]
    fn encrypted_keys_need_the_password() {
        let k = keys(Some("pw"));
        let key = load_secret_key(&BASE64.encode(&k.secret_text), Some("pw")).unwrap();
        let sig = sign_bytes(&key, b"a", "a", "1.0.0").unwrap();
        assert_eq!(verify_bytes(&k.pk, b"a", &sig).unwrap(), "1.0.0");
        assert!(load_secret_key(&k.secret_text, Some("wrong")).is_err());
        assert!(load_secret_key(&k.secret_text, None).is_err());
    }

    fn release(dir: &Path, k: &Keys, signed_as: &str) -> Vec<(String, PathBuf)> {
        let key = load_secret_key(&k.secret_text, None).unwrap();
        let mut out = Vec::new();
        for (target, name) in [
            ("windows-x86_64", "vgames_0.4.0_x64-setup.exe"),
            ("linux-x86_64", "vgames_0.4.0_amd64.AppImage"),
            ("darwin-aarch64", "vgames.app.tar.gz"),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, format!("{target} build")).unwrap();
            let sig = sign_bytes(&key, &std::fs::read(&path).unwrap(), name, signed_as).unwrap();
            std::fs::write(dir.join(format!("{name}.sig")), sig).unwrap();
            out.push((target.to_owned(), path));
        }
        out
    }

    #[test]
    fn manifest_verifies_every_artifact_and_verify_rechecks_it() {
        let dir = tempfile::tempdir().unwrap();
        let k = keys(None);
        let artifacts = release(dir.path(), &k, "0.4.0");
        let notes = dir.path().join("notes.txt");
        std::fs::write(
            &notes,
            "- You can now pin favorite packages to the top of your library.\n",
        )
        .unwrap();
        let pk = load_public_key(Some(&k.pubkey_config), Path::new("unused")).unwrap();
        let latest = manifest(&ManifestArgs {
            version: "0.4.0",
            notes: &notes,
            pub_date: "2026-10-02T12:00:00Z",
            url_base: "https://github.com/wouhliss/vgames/releases/download/desktop-v0.4.0",
            pubkey: &pk,
            artifacts: &artifacts,
        })
        .unwrap();
        assert_eq!(latest.platforms.len(), 3);
        assert_eq!(
            latest.platforms["linux-x86_64"].url,
            "https://github.com/wouhliss/vgames/releases/download/desktop-v0.4.0/vgames_0.4.0_amd64.AppImage"
        );
        let path = dir.path().join("latest.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&latest).unwrap()).unwrap();
        assert_eq!(verify(&path, dir.path(), &pk).unwrap(), 3);

        // A tampered artifact after the manifest was written.
        std::fs::write(dir.path().join("vgames.app.tar.gz"), "evil").unwrap();
        assert!(verify(&path, dir.path(), &pk).is_err());
    }

    #[test]
    fn manifest_refuses_wrong_versions_keys_and_targets() {
        let dir = tempfile::tempdir().unwrap();
        let k = keys(None);
        let notes = dir.path().join("notes.txt");
        std::fs::write(&notes, "- Fixed a crash.\n").unwrap();
        let pk = k.pk.clone();
        let args = |artifacts: &[(String, PathBuf)], pk: &minisign::PublicKey| {
            manifest(&ManifestArgs {
                version: "0.4.0",
                notes: &notes,
                pub_date: "2026-10-02T12:00:00Z",
                url_base: "https://example.com/d",
                pubkey: pk,
                artifacts,
            })
        };
        // Signed for another version (an older build replayed as 0.4.0).
        let old = release(dir.path(), &k, "0.3.0");
        assert!(
            args(&old, &pk)
                .unwrap_err()
                .to_string()
                .contains("signed for version 0.3.0")
        );
        // Verified against another key.
        let fresh = release(dir.path(), &k, "0.4.0");
        assert!(args(&fresh, &keys(None).pk).is_err());
        assert!(args(&fresh, &pk).is_ok());
        // Targets and URLs.
        assert!(parse_artifacts(&["windows-x86_64=a.exe".into()]).is_ok());
        assert!(parse_artifacts(&["linux-x86_64-appimage=a".into()]).is_ok());
        for bad in [
            "plan9-x86_64=a",
            "windows=a",
            "windows-x86_64",
            "linux-x86_64-snap=a",
        ] {
            assert!(parse_artifacts(&[bad.into()]).is_err(), "{bad}");
        }
        assert!(url_base("http://example.com").is_err());
        assert!(semver("0.4.0+build.1").is_err());
    }

    #[test]
    fn the_shipped_config_is_checked() {
        let conf =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../apps/desktop/src-tauri/tauri.conf.json");
        let err = load_public_key(None, &conf);
        // Until humans generate the updater key, releases must fail loudly.
        if let Err(e) = err {
            assert!(
                e.to_string().contains("placeholder") || e.to_string().contains("pubkey"),
                "{e:#}"
            );
        }
    }
}
