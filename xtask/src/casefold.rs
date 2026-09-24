//! Generates `crates/vgames-core/src/paths/casefold.rs` (Unicode simple case
//! folding: CaseFolding.txt statuses C and S). Changing the table changes which
//! manifests are valid, so a regeneration belongs in a `contract:` PR.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};

pub fn generate(input: &Path, root: &Path) -> Result<()> {
    let text =
        std::fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?;
    let out = render(&text)?;
    let target = root.join("crates/vgames-core/src/paths/casefold.rs");
    std::fs::write(&target, out).with_context(|| format!("writing {}", target.display()))?;
    println!("wrote {}", target.display());
    Ok(())
}

fn render(text: &str) -> Result<String> {
    let header = text
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches(['#', ' '])
        .trim();
    let Some(version) = header
        .strip_prefix("CaseFolding-")
        .and_then(|v| v.strip_suffix(".txt"))
    else {
        bail!("first line should be `# CaseFolding-<version>.txt`, found {header:?}");
    };
    let mut pairs = Vec::new();
    for line in text.lines() {
        let data = line.split('#').next().unwrap_or("").trim();
        if data.is_empty() {
            continue;
        }
        let fields: Vec<&str> = data.split(';').map(str::trim).collect();
        let [code, status, mapping, ..] = fields.as_slice() else {
            bail!("malformed line {line:?}");
        };
        if matches!(*status, "C" | "S") {
            let from = u32::from_str_radix(code, 16).with_context(|| format!("code {code}"))?;
            let to =
                u32::from_str_radix(mapping, 16).with_context(|| format!("mapping {mapping}"))?;
            pairs.push((from, to));
        }
    }
    pairs.sort_unstable();
    if pairs.windows(2).any(|w| matches!(w, [a, b] if a.0 == b.0)) {
        bail!("duplicate source code point");
    }
    let mut out = String::new();
    writeln!(
        out,
        "// @generated from {header} (Unicode simple case folding: statuses C and S)."
    )?;
    writeln!(
        out,
        "// Regenerate with `cargo xtask casefold <CaseFolding.txt>`. Do not edit by hand."
    )?;
    writeln!(
        out,
        "// Changing this table changes which manifests are valid: bump it only in a `contract:` PR."
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "pub const UNICODE_CASE_FOLDING_VERSION: &str = \"{version}\";"
    )?;
    writeln!(out)?;
    writeln!(out, "/// Sorted by source code point.")?;
    writeln!(
        out,
        "pub(super) static SIMPLE_CASE_FOLDING: [(u32, u32); {}] = [",
        pairs.len()
    )?;
    for (a, b) in &pairs {
        writeln!(out, "    (0x{a:04X}, 0x{b:04X}),")?;
    }
    writeln!(out, "];")?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_statuses_c_and_s_only() {
        let src = "# CaseFolding-18.0.0.txt\n# comment\n0041; C; 0061; # A\n00DF; F; 0073 0073; # ß\n1E9E; S; 00DF; # ẞ\n0130; T; 0069; # İ\n";
        let out = render(src).unwrap();
        assert!(out.contains("\"18.0.0\""));
        assert!(out.contains("(0x0041, 0x0061)"));
        assert!(out.contains("(0x1E9E, 0x00DF)"));
        assert!(!out.contains("0x00DF, 0x0073"));
        assert!(!out.contains("0x0130"));
        assert!(out.contains("[(u32, u32); 2]"));
    }
}
