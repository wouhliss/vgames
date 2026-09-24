# Agent 5 status

DevOps & Security (`crates/vgames-core`, `crates/vgames-cli`, `xtask`, `.github/**`, `deny.toml`,
`apps/desktop/src-tauri/src/updater/**`, `docs/security/**`, `runtimes/**`, `scripts/release-notes/**`,
`.changes/` tooling).

## Done

## In progress
- A5-T03 (first slice, critical path): `vgames_core::sign` — key ids, `VG1-…` fingerprints, domain-separated
  signatures, `vgames.sig/1` envelopes. Lands first because it unblocks `/.well-known/vgames.json` (Agent 1).
- Next, in order: A5-T01 CI → A5-T02 paths / layout / manifest → A5-T04 trust + `verify_manifest` →
  A5-T03 keyfile + WASM → A5-T02 compat.

## Interfaces delivered (other agents may now rely on these)

## Needs from others

## Blockers / contract questions
