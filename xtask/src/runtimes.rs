//! Runtime catalog (09-compatibility §5, A5-T12): `runtimes/catalog.toml` →
//! `runtimes.json` (`vgames.runtimes/1`) → minisign signature.
//!
//! - `build`: validates the catalog with `vgames_core::runtimes` (the parser the
//!   launcher uses), enforces the non-commercial tripwire, and numbers the output
//!   `previous + 1` so versions only go up. The output is deterministic (sorted
//!   entries, compact JSON) apart from `generated_at`.
//! - `sign`: minisign (prehashed) with the runtime-catalog key; writes `.minisig`.
//! - `verify`: `vgames_core::runtimes::verify_catalog` against a public key.
//! - `upsert`: adds entries found by the upstream watcher (`runtimes.yml`) and
//!   rewrites `catalog.toml` in its canonical form. A pinned entry never changes:
//!   the same id + version + os + arch with other bytes is refused.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use vgames_core::Timestamp;
use vgames_core::runtimes::{
    self, Arch, ArchiveFormat, Catalog, Os, Redistribution, Runtime, RuntimeId,
};

/// `runtimes/catalog.toml`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogToml {
    /// Required and explicit: the tripwire for non-commercial entries (D3DMetal).
    pub commercial: bool,
    #[serde(default, rename = "runtime", skip_serializing_if = "Vec::is_empty")]
    pub runtimes: Vec<RuntimeToml>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeToml {
    pub id: RuntimeId,
    pub version: String,
    pub os: Os,
    pub arch: Arch,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub archive: ArchiveFormat,
    pub license: String,
    pub min_launcher_version: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rosetta_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub macos_max_supported: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redistribution: Option<Redistribution>,
}

impl RuntimeToml {
    fn key(&self) -> (RuntimeId, Os, Arch, &str) {
        (self.id, self.os, self.arch, &self.version)
    }

    fn describe(&self) -> String {
        let os = format!("{:?}", self.os).to_lowercase();
        let arch = format!("{:?}", self.arch).to_lowercase();
        format!("{} {} ({os}, {arch})", self.id.as_str(), self.version)
    }
}

impl From<RuntimeToml> for Runtime {
    fn from(r: RuntimeToml) -> Self {
        Self {
            id: r.id,
            version: r.version,
            os: r.os,
            arch: r.arch,
            url: r.url,
            sha256: r.sha256,
            size: r.size,
            archive: r.archive,
            license: r.license,
            min_launcher_version: r.min_launcher_version,
            rosetta_required: r.rosetta_required,
            macos_max_supported: r.macos_max_supported,
            redistribution: r.redistribution,
        }
    }
}

pub fn read_toml(path: &Path) -> Result<CatalogToml> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// The comment block `upsert` writes at the top of `catalog.toml`.
const HEADER: &str = include_str!("runtimes_header.toml");

/// `catalog.toml` in its canonical form: the header, then the entries sorted
/// like `runtimes.json`.
pub fn to_canonical_toml(catalog: &CatalogToml) -> Result<String> {
    let mut sorted = CatalogToml {
        commercial: catalog.commercial,
        runtimes: catalog.runtimes.clone(),
    };
    sorted.runtimes.sort_by(|a, b| a.key().cmp(&b.key()));
    Ok(format!("{HEADER}{}", toml::to_string(&sorted)?))
}

/// Adds `entries` to `catalog`; returns a line per added entry. Entries already
/// pinned identically are skipped; a pinned entry with other bytes is an error.
pub fn upsert(catalog: &mut CatalogToml, entries: Vec<RuntimeToml>) -> Result<Vec<String>> {
    let mut added = Vec::new();
    for entry in entries {
        match catalog.runtimes.iter().find(|r| r.key() == entry.key()) {
            Some(existing) if *existing == entry => {}
            Some(existing) => bail!(
                "{} is already pinned with other values (sha256 {} → {}): an upstream asset \
                 changed under the same version; investigate before pinning anything",
                entry.describe(),
                existing.sha256,
                entry.sha256
            ),
            None => {
                added.push(entry.describe());
                catalog.runtimes.push(entry);
            }
        }
    }
    Ok(added)
}

/// Builds `runtimes.json` bytes. `previous`: the last published `runtimes.json`.
pub fn build(
    toml: CatalogToml,
    previous: Option<&[u8]>,
    generated_at: Timestamp,
) -> Result<Vec<u8>> {
    let non_commercial: Vec<&str> = toml
        .runtimes
        .iter()
        .filter(|r| r.redistribution == Some(Redistribution::NonCommercial))
        .map(|r| r.id.as_str())
        .collect();
    if toml.commercial && !non_commercial.is_empty() {
        bail!(
            "catalog.toml says commercial = true, but {} may only be redistributed non-commercially \
             (09-compatibility §7): remove them in the same change",
            non_commercial.join(", ")
        );
    }
    let version = match previous {
        Some(bytes) => {
            let prev = runtimes::parse_and_validate(bytes)
                .context("the previous runtimes.json is invalid")?;
            prev.version
                .checked_add(1)
                .context("catalog version overflow")?
        }
        None => 1,
    };
    let mut entries: Vec<Runtime> = toml.runtimes.into_iter().map(Runtime::from).collect();
    entries.sort_by(|a, b| (a.id, a.os, a.arch, &a.version).cmp(&(b.id, b.os, b.arch, &b.version)));
    let catalog = Catalog {
        format: runtimes::FORMAT.to_owned(),
        version,
        generated_at,
        commercial: toml.commercial,
        runtimes: entries,
    };
    let bytes = serde_json::to_vec(&catalog)?;
    // The launcher's own rules, on the exact bytes it will receive.
    runtimes::parse_and_validate(&bytes).map_err(|e| anyhow::anyhow!("catalog.toml: {e}"))?;
    Ok(bytes)
}

/// Signs `runtimes.json` bytes: the `.minisig` text (prehashed).
pub fn sign(key: &minisign::SecretKey, data: &[u8], version: u64) -> Result<String> {
    let signature = minisign::sign(
        None,
        key,
        std::io::Cursor::new(data),
        Some(&format!("vgames.runtimes/1 version:{version}")),
        Some("vgames runtime catalog"),
    )
    .context("signing")?;
    Ok(signature.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOML: &str = r#"
commercial = false

[[runtime]]
id = "d3dmetal"
version = "3.0"
os = "macos"
arch = "aarch64"
url = "https://github.com/wouhliss/vgames/releases/download/runtimes/D3DMetal-3.0.tar.gz"
sha256 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
size = 90000000
archive = "tar.gz"
license = "LicenseRef-Apple-GPTK"
min_launcher_version = "0.2.0"
redistribution = "non-commercial"
macos_max_supported = 27

[[runtime]]
id = "ge-proton"
version = "GE-Proton10-17"
os = "linux"
arch = "x86_64"
url = "https://github.com/GloriousEggroll/proton-ge-custom/releases/download/GE-Proton10-17/GE-Proton10-17.tar.gz"
sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
size = 480000000
archive = "tar.gz"
license = "BSD-3-Clause AND LGPL-2.1-or-later"
min_launcher_version = "0.2.0"
"#;

    fn now() -> Timestamp {
        "2026-09-25T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn builds_sorted_monotonic_catalogs_the_launcher_accepts() {
        let first = build(toml::from_str(TOML).unwrap(), None, now()).unwrap();
        let c = runtimes::parse_and_validate(&first).unwrap();
        assert_eq!(c.version, 1);
        assert_eq!(c.runtimes[0].id, RuntimeId::GeProton, "sorted by id");
        let second = build(toml::from_str(TOML).unwrap(), Some(&first), now()).unwrap();
        assert_eq!(runtimes::parse_and_validate(&second).unwrap().version, 2);
        // Deterministic apart from the version.
        let again = build(toml::from_str(TOML).unwrap(), None, now()).unwrap();
        assert_eq!(first, again);
    }

    #[test]
    fn the_commercial_flag_is_a_tripwire() {
        let flipped = TOML.replace("commercial = false", "commercial = true");
        let err = build(toml::from_str(&flipped).unwrap(), None, now()).unwrap_err();
        assert!(err.to_string().contains("d3dmetal"), "{err:#}");
        // The flag must be written down.
        let missing = TOML.replace("commercial = false", "");
        assert!(toml::from_str::<CatalogToml>(&missing).is_err());
    }

    #[test]
    fn invalid_entries_fail_the_build() {
        let bad = TOML.replace(
            "https://github.com/GloriousEggroll",
            "http://github.com/GloriousEggroll",
        );
        let err = build(toml::from_str(&bad).unwrap(), None, now()).unwrap_err();
        assert!(err.to_string().contains("url"), "{err:#}");
        let dup = format!(
            "{TOML}\n{}",
            &TOML[TOML.find("[[runtime]]\nid = \"ge-proton\"").unwrap()..]
        );
        let err = build(toml::from_str(&dup).unwrap(), None, now()).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err:#}");
    }

    #[test]
    fn the_committed_catalog_is_canonical() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../runtimes/catalog.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        let catalog: CatalogToml = toml::from_str(&text).unwrap();
        assert_eq!(to_canonical_toml(&catalog).unwrap(), text);
    }

    #[test]
    fn upsert_adds_new_entries_and_never_changes_a_pin() {
        let mut catalog: CatalogToml = toml::from_str(TOML).unwrap();
        let existing = catalog.runtimes[1].clone();
        let mut newer = existing.clone();
        newer.version = "GE-Proton10-18".into();
        newer.url = newer.url.replace("10-17", "10-18");
        newer.sha256 = "c".repeat(64);
        // Already pinned identically: skipped. New version: added.
        let added = upsert(&mut catalog, vec![existing.clone(), newer.clone()]).unwrap();
        assert_eq!(added, vec![newer.describe()]);
        assert_eq!(catalog.runtimes.len(), 3);
        // The same version with other bytes is refused, and nothing is added.
        let mut swapped = existing.clone();
        swapped.sha256 = "d".repeat(64);
        let err = upsert(&mut catalog, vec![swapped]).unwrap_err();
        assert!(err.to_string().contains("already pinned"), "{err:#}");
        assert_eq!(catalog.runtimes.len(), 3);
        // The canonical form round-trips and builds.
        let text = to_canonical_toml(&catalog).unwrap();
        assert!(text.starts_with(HEADER));
        let reparsed: CatalogToml = toml::from_str(&text).unwrap();
        assert_eq!(to_canonical_toml(&reparsed).unwrap(), text);
        build(reparsed, None, now()).unwrap();
    }

    #[test]
    fn signed_output_verifies_with_the_launcher_code() {
        let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let bytes = build(toml::from_str(TOML).unwrap(), None, now()).unwrap();
        let sig = sign(&pair.sk, &bytes, 1).unwrap();
        let v = runtimes::verify_catalog(&bytes, &sig, &pair.pk.to_base64(), None).unwrap();
        assert_eq!(v.catalog.runtimes.len(), 2);
        assert!(sig.contains("trusted comment: vgames.runtimes/1 version:1"));
    }
}
