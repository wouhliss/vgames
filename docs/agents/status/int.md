# INT status

## Done
- INT-04 OpenAPI maintenance: [#149](https://github.com/wouhliss/vgames/pull/149) merged at `327d1af` after every hosted check and all three OS builds passed. utoipa 6, axum 0.3 and Swagger UI 10 are upgraded together; strict drift passes with an empty allowlist. #79–81 are closed with superseding commit links.
- INT-04 server security: [#148](https://github.com/wouhliss/vgames/pull/148) merged at `ad405e9` after all seventeen hosted checks, three OS builds and the final gate passed. F3 credentials are redacted; server F5 frame decoding is bounded and mutation-tested. Final readiness source passes 50 consecutive parallel groups (500 tests, unchanged ten-second deadlines). Stale-base #147 is closed as superseded.
- INT-03: [#145](https://github.com/wouhliss/vgames/pull/145) merged at `37aa9b6` after every hosted check and all three OS legs passed. The [required whole-launcher/DB job](https://github.com/wouhliss/vgames/actions/runs/37601243544/job/112725867496) took 9m05s; the [matrix gate](https://github.com/wouhliss/vgames/actions/runs/37601243523/job/112730646547) passed in 3s. Negative proofs #129–132 establish unit, chat, scope and failed-leg enforcement. Required-check documentation and checklist A3 match. INS-09 integration remains tracked until its harness lands.
- Integrator review / INS-03: [#144](https://github.com/wouhliss/vgames/pull/144) merged at `6069524` after twelve CI checks and all three OS builds passed. Adopted #138 with scoped cancellation cleanup and an in-flight claim regression; #138 closed as superseded. All eleven local gates and 312 whole-launcher tests passed.
- INT-02 ownership: [#127](https://github.com/wouhliss/vgames/pull/127) merged at `f49b82e` after all twelve [hosted checks](https://github.com/wouhliss/vgames/actions/runs/37511027812) passed. Six regressions cover dead patterns, valid pending paths and security rules; #124 was superseded without force-pushing. Parser contract merged separately in [#137](https://github.com/wouhliss/vgames/pull/137) at `0c129e2` after all twelve [hosted checks](https://github.com/wouhliss/vgames/actions/runs/37519356827) passed; #128/#134/#135 closed with that commit. INT-02 acceptance is complete.
- INT-01: [#118](https://github.com/wouhliss/vgames/pull/118) merged at `ef06cf1`: exact stable minimum/pin policy, weekly watcher, reviewed #103–106 adoption and the reproduced request-pool readiness fix. All [CI checks](https://github.com/wouhliss/vgames/actions/runs/37463345825) and all [desktop OS checks](https://github.com/wouhliss/vgames/actions/runs/37463345682) passed before merge. [Manual toolchain watcher](https://github.com/wouhliss/vgames/actions/runs/37465743936) green. Superseded #103–106/#111/#116 closed with commit links; #76/#77 closed with the merged TS 5/Node 22 major-ignore rationale.
- Adopted live phase-1 core, CLI, updater, security tooling and release automation at `d6d8798`; phase-1 status files remain frozen.
- Integrator loop: fixed real-API admin authentication and fixtures in [#108](https://github.com/wouhliss/vgames/pull/108), merged at `7b8116c`. Main [CI](https://github.com/wouhliss/vgames/actions/runs/37456548013) green (5m40s); [real E2E](https://github.com/wouhliss/vgames/actions/runs/37456550701) green (2m29s).
- INT-01 triage: closed #63 (superseded by `6e8f9ba`) and #68 (`ba00d98`), with comments.
- INT-01 mismatch proof: [#112](https://github.com/wouhliss/vgames/pull/112) rejected by the required Rust policy step in [37457143357](https://github.com/wouhliss/vgames/actions/runs/37457143357); closed without merging.

## In progress
- Main `327d1af` (#149) is validating in push CI; previous `ad405e9` push CI is green; previous `e4f0732` CI and manual E2E are green, including the 8 GiB install scenario. #147 closed after its reviewed current-base replacement #148 merged. Previous main `37aa9b6` (INT-03): [push CI](https://github.com/wouhliss/vgames/actions/runs/37602723125) is green. Previous main `6069524`: [push CI](https://github.com/wouhliss/vgames/actions/runs/37600335996) is green after the reviewed updater checkpoint merge. Prior main `deebba1`: [push CI](https://github.com/wouhliss/vgames/actions/runs/37534663887) and [scheduled CI](https://github.com/wouhliss/vgames/actions/runs/37594827481) are green. Latest [scheduled desktop matrix](https://github.com/wouhliss/vgames/actions/runs/37596490524) and [real E2E](https://github.com/wouhliss/vgames/actions/runs/37465748125) are green. The transient browser-download 403 on `0c129e2` cleared on subsequent unchanged browser installation; recovery dispatch was cancelled by the newer main push. No open Ready for INT, contract acknowledgement or Merged without INT lines remain after closing #138.
- Local restart reproduced a fixture collision in the admin authorization matrix: the synthetic Discord suffix had only 16 random bits. INT-02 replaces it with a process-wide counter and proves 100,000 unique synthetic IDs; authorization assertions are unchanged.
- INT-03: whole-launcher CI consolidation is next. INT-02’s merged package-parser contract records the existing path, ordering, empty-file, label, duplicate-key and count limits. Production parsing is unchanged; the public-parser count audit pins the documented 64/256 limits and passes. All local gates pass; after INS’s rename, affected TypeScript, 55 launcher cases, changelog/security and secrets checks passed again.
- INT-01 scheduled-run acceptance: [scheduled CI](https://github.com/wouhliss/vgames/actions/runs/37594827481) is green on `deebba1`; [scheduled desktop matrix](https://github.com/wouhliss/vgames/actions/runs/37596490524) is green. Manual runs remain distinct evidence.
- Full local CI on the maintenance commit: all gates pass, with Vitest workers bounded to two for this machine's memory. Unbounded local UI workers delayed a lazy route beyond its unchanged wait; the same assertions pass with the supported worker limit.
- Readiness regression passes and restoring the old request-pool listener fails at the original ten-second timeout. The first repeated run passed 34 times before concurrent suites collided on SQLx database names. On a dedicated PostgreSQL 18.6 instance, the same readiness fix passes 50 consecutive parallel runs (10 realtime/presence tests, 500 passes, eight test threads). Test binary built from `2c17c63`; the identical listener implementation is merged in #118.
- INT-03 CI/DB/matrix gate, INT-04 server hardening/history/utoipa, INT-06 contract and INT-07 hosted soak prepared on separate local branches while INT-01 OS validation ran; publish and merge in task order.
- #92 stays with INT-05; utoipa #79–81 with INT-04. #85 was superseded by INS's release selection at `9507913`.

## Interfaces delivered (other agents may now rely on these)
- Required launcher CI runs the entire crate under Xvfb, then the four chat/chaos scenarios against one PostgreSQL service; the same DB step adds install/publish/save targets when their files land. Every PR reports the always-running Desktop matrix gate (#145).
- Exact pinned stable policy: `cargo xtask toolchain check` (CI and local gate); weekly/manual `toolchain-watch.yml` maintains one upgrade issue (#118).
- Realtime LISTEN uses one bounded, cancellable connection independent of the request pool (#118); test startup deadlines remain ten seconds.
- `apps/admin-web/e2e/helpers.ts::signIn(page)` exercises the real fake-Discord web flow and caches session cookies in worker memory (#108).

## Needs from others

## Blockers / contract questions
- G1/F1 wiring is now on main from INS-03/04; INT-11 review remains; F2/F4/launcher F5 remain pending GAME; server F3/F5 are INT-04.
- INT-08: no file-sending tool has been discovered. No release keys generated; follow the prescribed no-generation fallback if still unavailable when reached.

## Built for you
- From INS (INS-08, INT-03 fallback): `e2e.yml` has a nightly `install` job running `tests/install_e2e.rs` at full scale
  (8 GiB, frees runner disk first). The small scale needs nothing new: `scripts/ci/desktop-db-tests.sh` already picks
  the test up in the `desktop` job.

## Security review (INT-01 dependency adoption)
- vodozemac 0.11.1 changes HPKE check-code derivation; vgames uses Olm/Megolm, not that HPKE interface. Crypto tests remain required. Reviewed upstream 0.11.0…0.11.1 source diff.
- tauri-plugin-updater 2.13.1 preserves signed-version and artifact verification, removes process-wide Linux certificate environment mutation, and otherwise changes documentation, dependencies and equivalent let-chain syntax. `requireSignedVersion` and `createUpdaterArtifacts` remain true. Reviewed upstream 2.12.0…2.13.1 production source diff.
- Action updates retain immutable SHA pins: install-action updates tool manifests/checksums; sbom-action updates Syft 1.51.1→1.54.0 and build tooling. Permissions stay unchanged. The blocking 1.6.2 pin is intact.
- Require every hosted check, including all three desktop OS legs, on the current base before merging. Local full CI reproduced the readiness flake: SQLx test pools share a global 20-connection parent, while realtime listeners permanently borrow request-pool connections; fixed by dedicated bounded/cancellable LISTEN connection; regression and 50 consecutive parallel group runs pass (INT-04); final server changes will repeat this proof.

## Package-format contract review (INT-02)
- The contract records existing parser behavior, including NFKC safety checks while preserving accepted NFC text, byte-wise order, empty-file hashing, labels and duplicate-key rejection. Production parsing is unchanged. Added boundary regressions accept the maximum and reject one over for launch targets, saves, patterns and environment entries; the whole core suite passes.


## Launcher CI consolidation prepared (INT-03)
- Full local validation passes: Rust 3m33s, SQLx 10s, WASM 7s, TypeScript 51s, launcher mock 2m35s, admin mock 1m00s, desktop 1m58s, supply chain 4s, secrets 2s and workflow lint. The desktop step runs 268 normal tests plus all four ignored chat/chaos scenarios; the existing prelaunch benchmark and soak remain in their separate purposes.
- Negative hosted proofs (all closed without merging): launcher unit [#129](https://github.com/wouhliss/vgames/pull/129) failed the required desktop job in [3m34s](https://github.com/wouhliss/vgames/actions/runs/37513426731/job/112440165074); chat assertion [#130](https://github.com/wouhliss/vgames/pull/130) failed its database step in [3m56s](https://github.com/wouhliss/vgames/actions/runs/37513431736/job/112440182763). Docs-only [#132](https://github.com/wouhliss/vgames/pull/132) passed the [gate in 3s](https://github.com/wouhliss/vgames/actions/runs/37513442865/job/112440457469) with every matrix leg skipped by scope. Failed Linux leg [#131](https://github.com/wouhliss/vgames/pull/131) caused the [final gate to fail in 3s](https://github.com/wouhliss/vgames/actions/runs/37513436013/job/112452523859), while Windows and macOS passed. Proof branches use the same proposed gate implementation.
- INS-08's `install_e2e` landed in #146 and is automatically included at PR scale. INS-09's real-application harness and the future publish/save scenario files are absent on the current base. The single database step automatically includes each named scenario when its file lands; add real-app nightly and PR coverage when the harness exists.

- INT-03 current-base validation: with INS’s install worker, 295 normal launcher tests and all four chat/chaos database scenarios pass under Xvfb; desktop 1m28s, changelog/security 2s, secrets 2s and workflow lint pass. Existing prelaunch benchmark and soak stay in their separate purposes.

## Updater checkpoint security review (INS-03 / #138)
- Adopted #138 on current main, retaining its signed-artifact verification, game-running checks and serialized install lock. Downloads stop at journaled checkpoints before installation; release signing configuration is unchanged.
- Replaced manual release calls with a scoped checkpoint guard: shutdown, missing updates, failed verification/install and cancellation of the install future all release the queue. Tests cover checkpoint ordering, shutdown, aborted futures and failed/missing-update exits. Successful installation keeps the guard through restart.
- Reviewed replacement #144 merged at `6069524` after every hosted check and all three OS legs passed; #138 is closed. Whole-launcher validation passed 312 tests, including the blocked database claim and updater cancellation cases.

- Queue review also found an in-flight SQLite claim could become active after the queue reported idle. Serialized the claim-and-register operation with entering the checkpoint; a real blocked-database regression proves checkpoint completion waits for that claim and leaves no active job. This is the smallest required change to INS's queue wiring for the updater boundary.

- INT-03 latest library-base validation (`deebba1`): 308 normal launcher tests plus all four chat/chaos database scenarios pass under Xvfb; required desktop step 2m47s, changelog/security 3s, secrets 2s, workflow lint clean. On `6069524`, 312 normal launcher tests and all four chat/chaos database scenarios pass under Xvfb; desktop 2m44s, changelog/security and secrets 2s each, workflow lint clean.

## Server hardening validation prepared (INT-04)
- `auth::tests::token_debug_redacts_credentials` passes; deliberately printing the synthetic code makes it fail with "credential leaked". Serialization still carries the real field values.
- Four `realtime::tests` decoder tests pass: arbitrary bytes, mutated valid frames, oversized valid frames and exact-limit/version boundaries. Removing the size bound fails `oversized_valid_realtime_frames_are_rejected` at `extra = 1`; no drift allowlist/check is relaxed.


## Server follow-up validation (INT-04)
- Final F3/F5 source `fb89f04` passed 50 consecutive parallel realtime/presence groups: 500 passes, eight test threads, 790.599 seconds on an isolated PostgreSQL 18.6 instance. Every readiness deadline remains ten seconds. Dedicated database and copied executable were removed after the run; evidence is recorded in the integrator recovery results.
- Manual auth Debug implementations redact authorization codes, verifiers and both token types. Realtime decoding rejects frames above 64 KiB and unsupported version envelopes without panicking; mutation checks proved both fixes necessary.

- INT-04 latest combined validation: F3/F5 Rust gate 3m50s, SQLx 12s, changelog/security and secrets 1s each on the latest library base. The utoipa trio also passes Rust/strict drift (3m48s) and SQLx (14s); the proposed history endpoint passes its paging/auth tests, strict drift and SQLx (3m50s/13s). Source changes are identical after the CI-only rebase.

- After the INT-03 rebase, the server branch passes the whole launcher plus all four DB scenarios (3m12s), changelog/security (2s), secrets and workflow lint (1s each). No backend source changed during this rebase. Compat history UI also passes all eleven units and all three browser cases in mock and real-API mode; its temporary real fixture was removed.

- INT-04 replacement after INS-08 #146: whole launcher suite, both install test functions and four chat/chaos scenarios pass; desktop gate 3m11s, changelog/security and secrets 2s each, workflow lint clean. Install scenario completes in 19.26s at PR scale. Admin draft foundation combined with the history UI passes all 150 unit tests and type checking.

- INT-04 utoipa 6 / axum 0.3 / swagger-ui 10 combined upgrade passes Rust/strict drift (3m53s), SQLx (13s), whole launcher and all install/chat/chaos scenarios (3m27s), changelog/security, supply-chain and secrets on the INS-08 base. The drift allowlist is empty. Main `e4f0732` CI and its manually dispatched E2E (including 8 GiB install) are green; manual matrix on `37aa9b6` is green. These are distinct from scheduled-night acceptance.
- INT-04 history endpoint on the install-test base: Rust/strict drift and authorization matrix pass (3m49s). Clearing the shared target’s stale protocol artifact resolved a missing new DTO in separate Cargo feature builds; with unchanged source, SQLx passes (11s), the whole launcher and install/chat/chaos scenarios pass (3m16s), and changelog/security plus secrets pass. Fresh hosted checks remain required.
