# INT status

## Done
- Adopted live phase-1 core, CLI, updater, security tooling and release automation at `d6d8798`; phase-1 status files remain frozen.
- Integrator loop: fixed real-API admin authentication and fixtures in [#108](https://github.com/wouhliss/vgames/pull/108), merged at `7b8116c`. Main [CI](https://github.com/wouhliss/vgames/actions/runs/37456548013) green (5m40s); [real E2E](https://github.com/wouhliss/vgames/actions/runs/37456550701) green (2m29s).
- INT-01 triage: closed #63 (superseded by `6e8f9ba`) and #68 (`ba00d98`), with comments.
- INT-01 mismatch proof: [#112](https://github.com/wouhliss/vgames/pull/112) rejected by the required Rust policy step in [37457143357](https://github.com/wouhliss/vgames/actions/runs/37457143357); closed without merging.

## In progress
- INT-01: Replacement for [#116](https://github.com/wouhliss/vgames/pull/116) enforces the exact stable minimum/pin, adds the stable watcher and npm major ignores, and adopts #103–106's reviewed changes. #111 was fully green, but the pre-merge fetch found main had moved to `fffe666`; #116 rebases it without force-pushing.
- Current base `9507913`: INS's release selection landed; [main CI](https://github.com/wouhliss/vgames/actions/runs/37459480482) green (5m48s). No ready/contract-ack/merged-without-INT lines in live status files.
- #116's first CI exposed #103's known 150 ms lightbox fade contrast race (4.41/4.45:1); adopt reduced motion rather than retrying an application failure. Perf measurements retain their original project settings. The frozen #103 phase-1 hunk recorded the same evidence and is omitted.
- Dependency adoption: npm units 275 desktop, 140 admin, 6 release-notes pass; lint/typecheck/OpenAPI lint, launcher mock 55 and admin mock 22 pass. Real admin ten active cases pass with four OAuth workers within the unchanged 20/minute auth budget; twelve existing mock-only cases remain. Real traces are disabled because they contain live test session cookies; assertions remain intact.
- Local Rust and whole desktop suites pass for the dependency group. User-space desktop toolchain now works without sudo; local desktop gate 2m22s and supply-chain gate 7s pass. Earlier full local run: Rust 7m28s, SQLx 33s, WASM 46s, TypeScript 33s, launcher mock 2m37s, admin mock 1m03s.
- Full all-ref local secret scan passes (342 commits). Pruning the deleted remote proof branch removed its stale tracking ref; no allowlist or check was weakened.
- #85 stays with INS-01; #92 with INT-05; utoipa #79–81 with INT-04. Original #103–106 stay open until the replacement merges, then close with its superseding commit. #76/#77 close once the major-ignore policy merges.
- INT-02 changes prepared locally: six ownership tests and core count-boundary test pass; contract remains separate from ownership implementation.

## Interfaces delivered (other agents may now rely on these)
- `apps/admin-web/e2e/helpers.ts::signIn(page)` exercises the real fake-Discord web flow and caches session cookies in worker memory (#108).

## Needs from others

## Blockers / contract questions
- G1/F1 remain pending INS-03/04; F2/F4/launcher F5 remain pending GAME; server F3/F5 are INT-04.
- INT-08: no file-sending tool has been discovered. No release keys generated; follow the prescribed no-generation fallback if still unavailable when reached.

## Built for you

## Security review (INT-01 dependency adoption)
- vodozemac 0.11.1 changes HPKE check-code derivation; vgames uses Olm/Megolm, not that HPKE interface. Crypto tests remain required. Reviewed upstream 0.11.0…0.11.1 source diff.
- tauri-plugin-updater 2.13.1 preserves signed-version and artifact verification, removes process-wide Linux certificate environment mutation, and otherwise changes documentation, dependencies and equivalent let-chain syntax. `requireSignedVersion` and `createUpdaterArtifacts` remain true. Reviewed upstream 2.12.0…2.13.1 production source diff.
- Action updates retain immutable SHA pins: install-action updates tool manifests/checksums; sbom-action updates Syft 1.51.1→1.54.0 and build tooling. Permissions stay unchanged. The blocking 1.6.2 pin is intact.
- Require every hosted check, including all three desktop OS legs, on the current base before merging. Local full CI reproduced the readiness flake: SQLx test pools share a global 20-connection parent, while realtime listeners permanently borrow request-pool connections; investigating the starvation fix under INT-04.
