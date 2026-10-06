# INT status

## Done
- Adopted the live phase-1 core, CLI, updater, security tooling and release pipeline on main at `d6d8798`; phase-1 status files remain frozen.

- Integrator loop: fixed real-API admin authentication and fixtures in [#108](https://github.com/wouhliss/vgames/pull/108), merged at `7b8116c`. All PR checks green; main real E2E [37456550701](https://github.com/wouhliss/vgames/actions/runs/37456550701) green (2m29s).
- INT-01 triage: closed #63 (superseded by `6e8f9ba`) and #68 (`ba00d98`) with explanatory comments.

## In progress
- INT-01: exact stable policy, weekly/manual stable watcher, deliberate npm major pins, then reduced-motion and dependency groups.
- Isolated worktree: `../vgames-int`, branch `int/p2-toolchain`.
- Latest main CI [37456548013](https://github.com/wouhliss/vgames/actions/runs/37456548013) green (5m46s); previous scheduled CI green (5m58s), Desktop matrix green (31m50s). No phase-2 ready/contract/merged-without-INT status lines exist yet.
- INT-01 negative proof: [#112](https://github.com/wouhliss/vgames/pull/112) failed at the required Rust Toolchain policy step in [37457143357](https://github.com/wouhliss/vgames/actions/runs/37457143357); closed without merging.
- #85 remains INS-01; #92 remains INT-05; utoipa #79–81 remains INT-04.
- Local full checks: Rust 7m28s, SQLx 33s, WASM 46s, TypeScript 33s passed; launcher mock 2m37s and admin mock 1m03s also passed. Desktop/supply-chain tools were absent; installing them in user space. The running script was edited before its final summary, causing a parser error; syntax check now passes. Full all-ref secret scan sees only the deliberate unmerged historical proof `d679450` on `origin/agent5/gitleaks-proof`; current HEAD history scan passes (no allowlist added).

## Interfaces delivered (other agents may now rely on these)

## Needs from others

## Blockers / contract questions
- Release gate G1 and F1 remain pending INS-03/04; F2/F4/launcher F5 remain pending GAME. Server F3/F5 are INT-04.
- INT-08: no file-sending tool is exposed in this session. No release keys generated; follow the prescribed fallback to INT-09 when reached.

## Built for you
