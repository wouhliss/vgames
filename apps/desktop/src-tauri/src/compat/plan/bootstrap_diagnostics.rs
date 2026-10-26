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

/// Only numeric PIDs selected by Wine bootstrap names cross into the sampler.
/// Neither the command text nor its arguments leave this function.
pub(crate) fn bootstrap_pids(commands: &str) -> Vec<u32> {
    commands
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim().split_once(char::is_whitespace)?;
            let command = command.to_ascii_lowercase();
            let pid = pid.parse::<u32>().ok()?;
            (pid > 1 && (command.contains("wineboot") || command.contains("wineserver")))
                .then_some(pid)
        })
        .take(4)
        .collect()
}

/// Public identifier-shaped sampled symbols only: no headers, paths, addresses,
/// command lines, thread names, environment values or function arguments.
pub(crate) fn bootstrap_wait_symbols(sample: &str) -> Vec<(String, u64)> {
    let mut counts = BTreeMap::<String, u64>::new();
    for line in sample.lines() {
        let Some((prefix, _)) = line.split_once(" (in ") else {
            continue;
        };
        let mut fields = prefix
            .split_whitespace()
            .filter(|word| !matches!(*word, "+" | "!" | ":" | "|"));
        let Some(count) = fields.next().and_then(|value| value.parse::<u64>().ok()) else {
            continue;
        };
        let Some(symbol) = fields.next() else {
            continue;
        };
        if fields.next().is_some()
            || symbol.len() > 80
            || symbol.is_empty()
            || !symbol
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || !symbol
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        {
            continue;
        }
        if counts.len() >= 256 && !counts.contains_key(symbol) {
            continue;
        }
        let entry = counts.entry(symbol.to_owned()).or_default();
        *entry = entry.saturating_add(count);
    }
    let mut symbols: Vec<_> = counts.into_iter().collect();
    symbols.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    symbols.truncate(64);
    symbols
}

#[test]
fn bootstrap_sampler_keeps_only_bounded_public_symbols_and_numeric_pids() {
    let input = "Process: --fixture-private-value\nPath: /private/fixture-private-value\n\
        + 72 __psynch_cvwait  (in libsystem_pthread.dylib) + 12 [0x123]\n\
        + ! 72 wine_server_call (in ntdll.so) + 32 [0x456]\n\
        + 3 wine_server_call (in ntdll.so) + 2 [0x789]\n\
        + 4 /private/fixture-private-value (in module)\n\
        + 9 get_parameter(\"fixture-private-value\") (in module)\n\
        Arguments: 99 fixture-private-value (in module)\n";
    let symbols = bootstrap_wait_symbols(input);
    assert_eq!(
        symbols,
        vec![
            ("wine_server_call".into(), 75),
            ("__psynch_cvwait".into(), 72)
        ]
    );
    assert!(!format!("{symbols:?}").contains("fixture-private-value"));
    let commands = "1 wineboot\n12 /runtime/bin/wine wineboot --fixture-private-value\n34 wineserver\n\
        nope wineboot\n56 unrelated --fixture-private-value\n78 wineboot\n90 wineboot\n91 wineboot";
    assert_eq!(bootstrap_pids(commands), vec![12, 34, 78, 90]);
    let large = (0..300)
        .map(|n| format!("+ 1 symbol_{n} (in module)\n"))
        .collect::<String>();
    assert_eq!(bootstrap_wait_symbols(&large).len(), 64);
}
