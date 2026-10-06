# PLAY status

## Done

## In progress

## Interfaces delivered (other agents may now rely on these)

## Needs from others

## Blockers / contract questions

## Built for you
- INT-01 prepares #103's reduced-motion setting for launcher mock accessibility scans, leaving the perf project unchanged. Updated Vitest imports use explicit `.ts` extensions with the matching no-emit compiler option to satisfy the newer loader. Adopt this note when the dependency triage PR merges.
- From INS (INS-04): appended to `events.rs` (append-only, README §5): the bus variants and UI events
  `InstallsChanged` (`installs-changed`) and `CollectionsChanged` (`collections-changed`). INS-03 appends
  `DownloadsChanged` (`downloads-changed`) the same way.
