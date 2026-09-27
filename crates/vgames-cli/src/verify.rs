//! `vgames verify`: checks a published release the way launchers do before
//! installing it (01-security §3.4, 02-package-format §7):
//!
//! 1. the server's current trust bundle, verified under the pinned root key;
//! 2. the release descriptor, then the manifest from its URL, which must have
//!    exactly the size and BLAKE3 the descriptor lists;
//! 3. `verify_manifest` in install mode: signed by a publisher key the bundle
//!    trusts now (not revoked, not expired), for this server, package, version,
//!    platform and sequence.

use std::net::IpAddr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use url::Url;
use vgames_core::Digest;
use vgames_core::verify::{ExpectedRelease, VerifyMode, verify_manifest};
use vgames_proto::discovery::ServerInfo;
use vgames_proto::packages::Platform;
use vgames_proto::versions::ReleaseDescriptor;

use crate::keys;
use crate::publish::{parse_platform, resolve_package};
use crate::server::ServerArgs;
use crate::session::Session;
use crate::trust_server::{current_bundle, envelope_from, pin_for};

/// Largest manifest the API hands out (02-package-format §6).
const MAX_MANIFEST: u64 = 256 * 1024 * 1024;

#[derive(Args)]
#[command(after_help = "\
Examples:
  vgames verify --server https://games.example.com --package my-game --platform windows-x86_64
  vgames verify --server … --package my-game --platform linux-x86_64 --json

Checks the current release as launchers do before installing it: the trust bundle under the
pinned root key, the manifest's size and hash, and its signature by a publisher key the bundle
trusts now. Exits 1 when a launcher would refuse the release, for example after its key was
revoked and before `vgames trust re-sign`.")]
pub struct VerifyArgs {
    #[command(flatten)]
    server: ServerArgs,
    /// Package id or slug.
    #[arg(long, value_name = "ID|SLUG")]
    package: String,
    /// windows-x86_64, windows-aarch64, linux-x86_64, linux-aarch64, macos-aarch64 or macos-x86_64.
    #[arg(long, value_parser = parse_platform)]
    platform: Platform,
    /// Print the result as JSON on stdout.
    #[arg(long)]
    json: bool,
    /// Root key file (public part) or base64 root public key, instead of the pinned one.
    #[arg(long)]
    root: Option<String>,
}

/// The manifest URL may be on another host (object storage): https only, or
/// plain http on a loopback address (the local stack).
fn manifest_url(s: &str) -> Result<Url> {
    let url = Url::parse(s).context("the descriptor's manifest URL is not a URL")?;
    let loopback = match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        None => false,
    };
    match url.scheme() {
        "https" => Ok(url),
        "http" if loopback => Ok(url),
        _ => bail!("the descriptor's manifest URL is not https"),
    }
}

/// Downloads exactly `size` bytes, stopping as soon as more arrive.
async fn download(url: Url, size: u64) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("building the download client")?;
    // The signed URL is never printed: errors name the step, not the URL.
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("downloading the manifest failed"))?;
    if !response.status().is_success() {
        bail!(
            "downloading the manifest failed with status {}",
            response.status()
        );
    }
    let mut body = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("downloading the manifest was interrupted"))?
    {
        if (body.len() + chunk.len()) as u64 > size {
            bail!("the manifest is larger than the descriptor says ({size} bytes)");
        }
        body.extend_from_slice(&chunk);
    }
    if body.len() as u64 != size {
        bail!(
            "the manifest has {} bytes, the descriptor says {size}",
            body.len()
        );
    }
    Ok(body)
}

pub async fn run(args: VerifyArgs) -> Result<()> {
    let session = Session::open(&args.server).await?;
    let info: ServerInfo = session.api.get("/.well-known/vgames.json").await?;
    let pin = pin_for(&session, args.root.as_deref(), &info)?;
    let Some(bundle) = current_bundle(&session.api, &pin, info.server_id).await? else {
        bail!("the server has no trust bundle: launchers install nothing from it");
    };
    let package_id = resolve_package(&session.api, &args.package).await?;
    let descriptor: ReleaseDescriptor = session
        .api
        .get(&format!(
            "/v1/packages/{package_id}/releases/{}",
            args.platform.as_str()
        ))
        .await?;
    if descriptor.package_id != package_id || descriptor.platform != args.platform {
        bail!("the server answered with a release of another package or platform");
    }

    let size = u64::try_from(descriptor.manifest.size)
        .ok()
        .filter(|s| (1..=MAX_MANIFEST).contains(s))
        .context("the descriptor lists an impossible manifest size")?;
    let bytes = download(manifest_url(&descriptor.manifest.url)?, size).await?;
    if Digest::of(&bytes).to_hex() != descriptor.manifest.blake3 {
        bail!("the manifest does not match the BLAKE3 the descriptor lists");
    }

    let envelope = envelope_from(&serde_json::to_value(&descriptor.signature)?)?;
    let expected = ExpectedRelease {
        server_id: info.server_id,
        package_id,
        version_id: descriptor.version_id,
        platform: descriptor
            .platform
            .as_str()
            .parse()
            .map_err(|_| anyhow::anyhow!("unknown platform in the descriptor"))?,
        sequence: u64::try_from(descriptor.sequence).context("negative sequence")?,
    };
    let mode = VerifyMode::Install {
        now: keys::now()?,
        allow_older: false,
    };
    let verified = verify_manifest(&bundle.state, &envelope, &bytes, &expected, None, mode)
        .with_context(|| {
            format!(
                "launchers would refuse {} {} for {}",
                args.package,
                descriptor.version_label,
                args.platform.as_str()
            )
        })?;

    let label = bundle
        .state
        .publisher(&verified.key_id)
        .map(|p| p.label.clone())
        .unwrap_or_default();
    if args.json {
        let out = serde_json::json!({
            "package_id": package_id,
            "version_id": descriptor.version_id,
            "version_label": descriptor.version_label,
            "platform": args.platform.as_str(),
            "sequence": descriptor.sequence,
            "key_id": verified.key_id.to_string(),
            "key_label": label,
            "trust_bundle_version": bundle.state.version(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "OK: {} {} for {} (#{}) is signed by {} ({label}), trusted by bundle v{}.",
            args.package,
            descriptor.version_label,
            args.platform.as_str(),
            descriptor.sequence,
            verified.key_id,
            bundle.state.version()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_urls_are_https_or_loopback() {
        assert!(manifest_url("https://storage.example.com/m?sig=1").is_ok());
        assert!(manifest_url("http://127.0.0.1:8080/m").is_ok());
        assert!(manifest_url("http://localhost:8080/m").is_ok());
        assert!(manifest_url("http://storage.example.com/m").is_err());
        assert!(manifest_url("file:///etc/passwd").is_err());
    }
}
