//! Test-only process classification; raw command lines never leave this boundary.

use std::collections::BTreeMap;

pub(crate) fn bootstrap_process_counts(commands: &str) -> BTreeMap<&'static str, usize> {
    let commands = commands.to_ascii_lowercase();
    [
        ("wineboot", "wineboot"),
        ("installer", "msiexec.exe"),
        ("mono_installer", "wine-mono"),
        ("wineserver", "wineserver"),
        ("services", "services.exe"),
        ("explorer", "explorer.exe"),
        ("registry_editor", "reg.exe"),
    ]
    .into_iter()
    .map(|(name, pattern)| {
        (
            name,
            commands
                .lines()
                .filter(|line| line.contains(pattern))
                .count(),
        )
    })
    .collect()
}

#[test]
fn bootstrap_diagnostics_only_emit_fixed_counts() {
    let input = "wineboot.exe --fixture-private-value\n/runtime/bin/wine wineboot --init\nmsiexec.exe wine-mono.msi\nexplorer.exe\nreg.exe add --fixture-private-value\n";
    let counts = bootstrap_process_counts(input);
    assert_eq!(counts.get("wineboot"), Some(&2));
    assert_eq!(counts.get("installer"), Some(&1));
    assert_eq!(counts.get("mono_installer"), Some(&1));
    assert_eq!(counts.get("explorer"), Some(&1));
    assert_eq!(counts.get("registry_editor"), Some(&1));
    assert_eq!(counts.get("wineserver"), Some(&0));
    let output = format!("{counts:?}");
    assert!(!output.contains("fixture-private-value"));
    assert!(!output.contains("msiexec.exe"));
}
