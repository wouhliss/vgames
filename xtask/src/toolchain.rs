//! Exact stable toolchain policy (INT-01).
use anyhow::{Context, Result, ensure};
use std::path::Path;

fn validate(manifest: &str, toolchain: &str) -> Result<()> {
    let manifest: toml::Value = toml::from_str(manifest)?;
    let toolchain: toml::Value = toml::from_str(toolchain)?;
    let minimum = manifest
        .get("workspace")
        .and_then(|v| v.get("package"))
        .and_then(|v| v.get("rust-version"))
        .and_then(toml::Value::as_str)
        .context("workspace.package.rust-version is missing")?;
    let pin = toolchain
        .get("toolchain")
        .and_then(|v| v.get("channel"))
        .and_then(toml::Value::as_str)
        .context("toolchain.channel is missing")?;
    let version = semver::Version::parse(pin).context("pin must be an exact stable version")?;
    ensure!(
        version.pre.is_empty() && version.build.is_empty(),
        "pin must be stable"
    );
    ensure!(
        minimum == pin,
        "workspace rust-version {minimum} differs from toolchain pin {pin}"
    );
    Ok(())
}

pub fn check(root: &Path) -> Result<()> {
    validate(
        &std::fs::read_to_string(root.join("Cargo.toml"))?,
        &std::fs::read_to_string(root.join("rust-toolchain.toml"))?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    const MANIFEST: &str = "[workspace.package]\nrust-version = \"1.99.0\"";
    #[test]
    fn matching_stable_pin_passes() {
        validate(MANIFEST, "[toolchain]\nchannel = \"1.99.0\"").unwrap();
    }
    #[test]
    fn mismatch_and_floating_or_prerelease_pin_fail() {
        for pin in ["1.98.0", "stable", "1.99.0-beta.1", "1.99.0+local"] {
            assert!(validate(MANIFEST, &format!("[toolchain]\nchannel = {pin:?}")).is_err());
        }
    }
    #[test]
    fn repository_policy_matches() {
        check(&Path::new(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap();
    }
}
