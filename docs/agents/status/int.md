# INT status

## Done
- INT-02 ownership: [#127](https://github.com/wouhliss/vgames/pull/127) merged at `f49b82e` after all twelve [hosted checks](https://github.com/wouhliss/vgames/actions/runs/37511027812) passed. Six regressions cover dead patterns, valid pending paths and security rules; #124 was superseded without force-pushing. Parser contract follows separately.
- INT-01: [#118](https://github.com/wouhliss/vgames/pull/118) merged at `ef06cf1`: exact stable minimum/pin policy, weekly watcher, reviewed #103–106 adoption and the reproduced request-pool readiness fix. All [CI checks](https://github.com/wouhliss/vgames/actions/runs/37463345825) and all [desktop OS checks](https://github.com/wouhliss/vgames/actions/runs/37463345682) passed before merge. [Manual toolchain watcher](https://github.com/wouhliss/vgames/actions/runs/37465743936) green. Superseded #103–106/#111/#116 closed with commit links; #76/#77 closed with the merged TS 5/Node 22 major-ignore rationale.
- Adopted live phase-1 core, CLI, updater, security tooling and release automation at `d6d8798`; phase-1 status files remain frozen.
- Integrator loop: fixed real-API admin authentication and fixtures in [#108](https://github.com/wouhliss/vgames/pull/108), merged at `7b8116c`. Main [CI](https://github.com/wouhliss/vgames/actions/runs/37456548013) green (5m40s); [real E2E](https://github.com/wouhliss/vgames/actions/runs/37456550701) green (2m29s).
- INT-01 triage: closed #63 (superseded by `6e8f9ba`) and #68 (`ba00d98`), with comments.
- INT-01 mismatch proof: [#112](https://github.com/wouhliss/vgames/pull/112) rejected by the required Rust policy step in [37457143357](https://github.com/wouhliss/vgames/actions/runs/37457143357); closed without merging.

## In progress
- Main `5f36ec4`: [CI](https://github.com/wouhliss/vgames/actions/runs/37513938040) green, including INS’s catalog, queue history and prefix helper. Latest [real E2E](https://github.com/wouhliss/vgames/actions/runs/37465748125) remains green. No Ready for INT, contract acknowledgement or Merged without INT lines in live slice status files.
- Local restart reproduced a fixture collision in the admin authorization matrix: the synthetic Discord suffix had only 16 random bits. INT-02 replaces it with a process-wide counter and proves 100,000 unique synthetic IDs; authorization assertions are unchanged.
- INT-02: separate package-parser contract records the existing path, ordering, empty-file, label, duplicate-key and count limits. Production parsing is unchanged; the new count-boundary regression and whole core suite pass. All local gates pass; after INS’s rename, affected TypeScript, 55 launcher cases, changelog/security and secrets checks passed again.
- INT-01 scheduled-run follow-up: next scheduled desktop/CI run remains to be observed; do not treat a manual run as a scheduled one.
- Full local CI on the maintenance commit: all gates pass, with Vitest workers bounded to two for this machine's memory. Unbounded local UI workers delayed a lazy route beyond its unchanged wait; the same assertions pass with the supported worker limit.
- Readiness regression passes and restoring the old request-pool listener fails at the original ten-second timeout. The first repeated run passed 34 times before concurrent suites collided on SQLx database names. On a dedicated PostgreSQL 18.6 instance, the same readiness fix passes 50 consecutive parallel runs (10 realtime/presence tests, 500 passes, eight test threads). Test binary built from `2c17c63`; the identical listener implementation is merged in #118.
- INT-03 CI/DB/matrix gate, INT-04 server hardening/history/utoipa, INT-06 contract and INT-07 hosted soak prepared on separate local branches while INT-01 OS validation ran; publish and merge in task order.
- #92 stays with INT-05; utoipa #79–81 with INT-04. #85 was superseded by INS's release selection at `9507913`.

## Interfaces delivered (other agents may now rely on these)
- Exact pinned stable policy: `cargo xtask toolchain check` (CI and local gate); weekly/manual `toolchain-watch.yml` maintains one upgrade issue (#118).
- Realtime LISTEN uses one bounded, cancellable connection independent of the request pool (#118); test startup deadlines remain ten seconds.
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
- Require every hosted check, including all three desktop OS legs, on the current base before merging. Local full CI reproduced the readiness flake: SQLx test pools share a global 20-connection parent, while realtime listeners permanently borrow request-pool connections; fixed by dedicated bounded/cancellable LISTEN connection; regression and 50 consecutive parallel group runs pass (INT-04); final server changes will repeat this proof.

## Package-format contract review (INT-02)
- The contract records existing parser behavior, including NFKC safety checks while preserving accepted NFC text, byte-wise order, empty-file hashing, labels and duplicate-key rejection. Production parsing is unchanged. Added boundary regressions accept the maximum and reject one over for launch targets, saves, patterns and environment entries; the whole core suite passes.

- INT-02 parser contract: #128 passed every hosted check and all three OS legs, but the pre-merge fetch found INS’s catalog/queue/prefix changes on main. `int/p2-package-format-current` replaces it without force-pushing; all local gates passed with catalog/queue changes, then affected desktop/security/secret checks run with the prefix helper. Close #128 only after the replacement merges.
