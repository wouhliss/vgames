# INT status

## Done
- INT-05: #92 adopted through #169/#170/#172/#173/#174; original closed with actual merge receipts. Final browser merge `56700fb108c12d996002286e50afd523c4e86e18`; every applicable hosted check passed. All 34 mock and 22 applicable real-API cases pass, including all nine new robustness cases. Consecutive scheduled main E2E greens: [37920344971](https://github.com/wouhliss/vgames/actions/runs/37920344971) and [38043765484](https://github.com/wouhliss/vgames/actions/runs/38043765484).
- INT-03 crash-resume follow-up: [#168](https://github.com/wouhliss/vgames/pull/168) merged at `ffdab6f` after every hosted check, all three OS legs and exact-head [E2E](https://github.com/wouhliss/vgames/actions/runs/38097746614) passed. The full 8 GiB scenario itself took 366.93s (7m00s including compilation); 5 GiB publishing took 72.01s (1m48s including compilation). Candidate real release launcher, real admin and key-pipeline jobs all passed.
- INT-04 complete: history screen replacement [#156](https://github.com/wouhliss/vgames/pull/156) merged at `db7c205` after all applicable hosted checks and the scoped desktop gate passed; stale-base #152 is closed. Together with #148/#149/#151, F3/server F5 mutation proofs, strict drift, fifty parallel readiness groups, signed API paging/auth tests and loading/error/paging/axe UI coverage meet INT-04 acceptance.
- INT-04 history API: [#151](https://github.com/wouhliss/vgames/pull/151) merged at `e8ced0c` after all seventeen checks and all three OS builds passed. Admin-only newest-first revisions include signer ids and timestamps; HMAC cursors are bound to package and target. API tests cover paging, cursor tampering, unknown targets, roles and unpublished packages.
- INT-04 OpenAPI maintenance: [#149](https://github.com/wouhliss/vgames/pull/149) merged at `327d1af` after every hosted check and all three OS builds passed. utoipa 6, axum 0.3 and Swagger UI 10 are upgraded together; strict drift passes with an empty allowlist. #79–81 are closed with superseding commit links.
- INT-04 server security: [#148](https://github.com/wouhliss/vgames/pull/148) merged at `ad405e9` after all seventeen hosted checks, three OS builds and the final gate passed. F3 credentials are redacted; server F5 frame decoding is bounded and mutation-tested. Final readiness source passes 50 consecutive parallel groups (500 tests, unchanged ten-second deadlines). Stale-base #147 is closed as superseded.
- INT-03: [#145](https://github.com/wouhliss/vgames/pull/145) merged at `37aa9b6` after every hosted check and all three OS legs passed. The [required whole-launcher/DB job](https://github.com/wouhliss/vgames/actions/runs/37601243544/job/112725867496) took 9m05s; the [matrix gate](https://github.com/wouhliss/vgames/actions/runs/37601243523/job/112730646547) passed in 3s. Negative proofs #129–132 establish unit, chat, scope and failed-leg enforcement. Required-check documentation and checklist A3 match. INS-09 real-release application coverage landed in #153 and is included in the nightly workflow; both parts passed in main E2E 37631363850 (13m11s including the uncached release build).
- Integrator review / INS-03: [#144](https://github.com/wouhliss/vgames/pull/144) merged at `6069524` after twelve CI checks and all three OS builds passed. Adopted #138 with scoped cancellation cleanup and an in-flight claim regression; #138 closed as superseded. All eleven local gates and 312 whole-launcher tests passed.
- INT-02 ownership: [#127](https://github.com/wouhliss/vgames/pull/127) merged at `f49b82e` after all twelve [hosted checks](https://github.com/wouhliss/vgames/actions/runs/37511027812) passed. Six regressions cover dead patterns, valid pending paths and security rules; #124 was superseded without force-pushing. Parser contract merged separately in [#137](https://github.com/wouhliss/vgames/pull/137) at `0c129e2` after all twelve [hosted checks](https://github.com/wouhliss/vgames/actions/runs/37519356827) passed; #128/#134/#135 closed with that commit. INT-02 acceptance is complete.
- INT-01: [#118](https://github.com/wouhliss/vgames/pull/118) merged at `ef06cf1`: exact stable minimum/pin policy, weekly watcher, reviewed #103–106 adoption and the reproduced request-pool readiness fix. All [CI checks](https://github.com/wouhliss/vgames/actions/runs/37463345825) and all [desktop OS checks](https://github.com/wouhliss/vgames/actions/runs/37463345682) passed before merge. [Manual toolchain watcher](https://github.com/wouhliss/vgames/actions/runs/37465743936) green. Superseded #103–106/#111/#116 closed with commit links; #76/#77 closed with the merged TS 5/Node 22 major-ignore rationale.
- Adopted live phase-1 core, CLI, updater, security tooling and release automation at `d6d8798`; phase-1 status files remain frozen.
- Integrator loop: fixed real-API admin authentication and fixtures in [#108](https://github.com/wouhliss/vgames/pull/108), merged at `7b8116c`. Main [CI](https://github.com/wouhliss/vgames/actions/runs/37456548013) green (5m40s); [real E2E](https://github.com/wouhliss/vgames/actions/runs/37456550701) green (2m29s).
- INT-01 triage: closed #63 (superseded by `6e8f9ba`) and #68 (`ba00d98`), with comments.
- INT-01 mismatch proof: [#112](https://github.com/wouhliss/vgames/pull/112) rejected by the required Rust policy step in [37457143357](https://github.com/wouhliss/vgames/actions/runs/37457143357); closed without merging.

## In progress
- Integrator loop 2026-10-11: current main `54ce554` includes complete INT-05 adoption, signing contract #175 and updater version fix #176. Current push CI is green; #176 passed every hosted check and all three OS legs. Full main E2E on the completed browser source [38132572358](https://github.com/wouhliss/vgames/actions/runs/38132572358) is green. GAME started with #177/#178; the additive compatibility contract is adopted on current main before the next INT-06 publication. No Ready for INT, contract ack or Merged without INT entry is pending. No production release key or tag has been generated.
- Local restart reproduced a fixture collision in the admin authorization matrix: the synthetic Discord suffix had only 16 random bits. INT-02 replaces it with a process-wide counter and proves 100,000 unique synthetic IDs; authorization assertions are unchanged.
- INT-01 scheduled-run acceptance: [scheduled CI](https://github.com/wouhliss/vgames/actions/runs/37594827481) is green on `deebba1`; [scheduled desktop matrix](https://github.com/wouhliss/vgames/actions/runs/37596490524) is green. Manual runs remain distinct evidence.
- Full local CI on the maintenance commit: all gates pass, with Vitest workers bounded to two for this machine's memory. Unbounded local UI workers delayed a lazy route beyond its unchanged wait; the same assertions pass with the supported worker limit.
- Readiness regression passes and restoring the old request-pool listener fails at the original ten-second timeout. The first repeated run passed 34 times before concurrent suites collided on SQLx database names. On a dedicated PostgreSQL 18.6 instance, the same readiness fix passes 50 consecutive parallel runs (10 realtime/presence tests, 500 passes, eight test threads). Test binary built from `2c17c63`; the identical listener implementation is merged in #118.
- INT-04 is complete through #148/#149/#151/#156. INT-05 admin robustness, INT-06 release changes and INT-07 hosted soak are prepared on separate branches; publish and merge in task order after the main-health repair.
- #92 is closed after complete INT-05 adoption; utoipa #79–81 are closed after #149. #85 was superseded by INS's release selection at `9507913`.

## Interfaces delivered (other agents may now rely on these)
- Admin compatibility history: `GET /v1/admin/packages/{package_id}/compat/{target}` and regenerated client (#151); signed paging is available for unpublished packages.
- Required launcher CI runs the entire crate under Xvfb, then the four chat/chaos scenarios against one PostgreSQL service; the same DB step adds install/publish/save targets when their files land. Every PR reports the always-running Desktop matrix gate (#145).
- Exact pinned stable policy: `cargo xtask toolchain check` (CI and local gate); weekly/manual `toolchain-watch.yml` maintains one upgrade issue (#118).
- Realtime LISTEN uses one bounded, cancellable connection independent of the request pool (#118); test startup deadlines remain ten seconds.
- `apps/admin-web/e2e/helpers.ts::signIn(page)` exercises the real fake-Discord web flow and caches session cookies in worker memory (#108).

## Needs from others
- INS #119 merged at `72e35dc`; the shared auth hook is available for GAME’s social client. INT-11 will review its consumers and transport bounds.

## Blockers / contract questions
- INT-06 fixture containment: four actual HTTP/path regressions pass, including metadata symlink refusal before reading and readiness requests excluded from application receipts. Removing the metadata containment guard fails the new regression; restoration passes all four. This proves the fixture boundary; the bundled signed install/restart round trip still requires a hosted run and is not claimed green.
- INT-06 proposed shared updater preflight: the unchanged committed placeholder is correctly refused. An existing public test vector is accepted; disabling signed-version binding or artifact creation is refused. All three negative probes restore the original configuration and generate no private key. The real main dry run remains dependent on INT-08's committed public key; a candidate public-vector run cannot satisfy that acceptance item.
- G1/F1 wiring is now on main from INS-03/04; INT-11 review remains; F2/F4/launcher F5 remain pending GAME; server F3/F5 are INT-04.
- INT-08: no file-sending tool has been discovered. No release keys generated; follow the prescribed no-generation fallback if still unavailable when reached.

## Built for you
- From INS (INS-08, INT-03 fallback): `e2e.yml` has a nightly `install` job running `tests/install_e2e.rs` at full scale
  (8 GiB, frees runner disk first). The small scale needs nothing new: `scripts/ci/desktop-db-tests.sh` already picks
  the test up in the `desktop` job.
- From INS (INS-09, INT-03 fallback): `e2e.yml` has a nightly `launcher` job running `apps/desktop/e2e-real/run.sh 1,2`
  (release launcher, real API behind https://localhost). Part 1 is not on PRs: the uncached release build alone is
  over the 10 minutes INS-09 allows; wire it into `ci.yml` if you get the build cached under that.

- From INS (INS-06, INT-03 fallback): `tests/publish_e2e.rs` (launcher publishing through the real API in process)
  runs on PRs at 2 GiB through `scripts/ci/desktop-db-tests.sh` (already listed there), and nightly at 5 GiB as a
  second step of the `install` job in `e2e.yml` (`VGAMES_PUBLISH_E2E_GIB=5`).

- From INS (INS-07): `deny.toml` gains a license exception for `winx` (`Apache-2.0 WITH LLVM-exception`, like
  `target-lexicon`), the Windows-only dependency of `cap-std`, which install writes now use for handle-relative
  opens (Q12). Supply-chain config is yours: adjust freely.

- From INS (INS-05): `docs/security/test-matrix.md`, row "Compromised server → read tokens for other servers", now
  names `servers/tests.rs::a_request_to_one_server_never_carries_another_servers_token` (two servers signed in; each
  receives only its own tokens after a refresh, a switch with reconnect, and a restart), and "Gaps being closed" is
  empty. Security docs are yours: edit freely.

- From INS (INS-07, INT-03 fallback): `.github/workflows/transfer-budgets.yml` re-measures the download budgets
  (00-overview §7) on release builds on `ubuntu-24.04`, `windows-2025` and `macos-15`: loopback throughput, peak
  memory at 32 connections and disk footprint, printed to the job summary. Runs on PRs touching `vgames-transfer` and
  on demand; not a required check. Fold it into the matrix or nightly if you prefer.

## G1 re-review request (from INS, INS-10 → INT-11)
INS asks for the G1 re-review of the install chain (checklist items 1.1, 1.2 and 2.3), at `e8cfee8`:
- **Install:** the worker verifies the release with [`install::fetch_release`](https://github.com/wouhliss/vgames/blob/e8cfee8a18e2726ed9539dac4cfee9f62915585f/apps/desktop/src-tauri/src/downloads/job.rs#L282)
  (refreshing the trust bundle once on an unknown key) before [`install::install`](https://github.com/wouhliss/vgames/blob/e8cfee8a18e2726ed9539dac4cfee9f62915585f/apps/desktop/src-tauri/src/downloads/job.rs#L181)
  writes anything ([#133](https://github.com/wouhliss/vgames/pull/133)); refused keys write nothing (`an_untrusted_signing_key_writes_nothing`).
- **Resume:** startup requeue and the journal ([#133](https://github.com/wouhliss/vgames/pull/133); `a_restart_mid_install_resumes_from_the_journal`,
  and the real SIGKILL in `tests/install_e2e.rs`, [#146](https://github.com/wouhliss/vgames/pull/146)).
- **Update:** interrupted commits recovered with [`commit::recover_pending`](https://github.com/wouhliss/vgames/blob/e8cfee8a18e2726ed9539dac4cfee9f62915585f/apps/desktop/src-tauri/src/downloads/maintain.rs#L106), the
  installed release re-verified with [`load_local_release`](https://github.com/wouhliss/vgames/blob/e8cfee8a18e2726ed9539dac4cfee9f62915585f/apps/desktop/src-tauri/src/downloads/maintain.rs#L147), then
  [`update_safe`](https://github.com/wouhliss/vgames/blob/e8cfee8a18e2726ed9539dac4cfee9f62915585f/apps/desktop/src-tauri/src/downloads/maintain.rs#L230) ([#140](https://github.com/wouhliss/vgames/pull/140)).
- **Repair:** [`verify_install`](https://github.com/wouhliss/vgames/blob/e8cfee8a18e2726ed9539dac4cfee9f62915585f/apps/desktop/src-tauri/src/downloads/maintain.rs#L352) then [`repair_safe`](https://github.com/wouhliss/vgames/blob/e8cfee8a18e2726ed9539dac4cfee9f62915585f/apps/desktop/src-tauri/src/downloads/maintain.rs#L383).
- **F1:** [`adopt_resigned`](https://github.com/wouhliss/vgames/blob/e8cfee8a18e2726ed9539dac4cfee9f62915585f/apps/desktop/src-tauri/src/downloads/maintain.rs#L401), test `verify_adopts_a_resigned_manifest_without_downloading`
  (revoked key → launch refused → verify adopts the re-signed envelope, no pack byte fetched).
- In flight: [#153](https://github.com/wouhliss/vgames/pull/153) makes the worker fetch a first trust bundle before refusing (`Ok(None)` → one refresh),
  same rule as the unknown-key refresh.

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
- INS-08's `install_e2e` landed in #146 and is automatically included at PR scale. INS-09's real-application harness landed in #153 and already has one nightly job; publish/save scenario files remain absent on the current base. The single database step automatically includes each named scenario when its file lands; add real-app nightly and PR coverage when the harness exists.

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

- INT-04 history screen: full local TypeScript gate passes (51s), with all 25 admin mock browser cases (1m04s). Six new history units cover loading/empty, paging, malformed responses and 401/403/400; the three new browser cases include serious/critical axe scans. Full real-API validation passes: 13 browser cases (8.6s), including all three new history cases; twelve pre-existing mock-only cases retain their original real-mode exclusions. Temporary fixture keys, server process and database were removed. Paging browser cases inject controlled responses; actual endpoint paging/signature/auth behavior is proved by the Rust API tests.
- Integrator review of INS startup fix [#150](https://github.com/wouhliss/vgames/pull/150): entering Tauri’s own Tokio runtime for setup addresses the release startup panic found by INS-09 without a new runtime or IPC/security changes. It merged at `e8cfee8`; main CI is green on the fix and the subsequent harness. Hosted idle-soak preparation will include it.

- INT-05 scheduled acceptance: [37920344971](https://github.com/wouhliss/vgames/actions/runs/37920344971) and [38043765484](https://github.com/wouhliss/vgames/actions/runs/38043765484) are two consecutive green scheduled main E2E runs, including install, publishing and real release launcher jobs; manual dispatches are separate evidence.

- INT-04 history screen on the INS-09 harness base (`2dfa6d0`): full TypeScript gate passes (49s), whole-launcher plus install/chat/chaos database checks pass (3m43s), changelog/security passes (3s) and gitleaks passes (2s). The screen source is unchanged from its 25 mock and 13 real-API browser passes; hosted checks are repeated on the refreshed base.

## Main health repair (integrator loop, INS-08)
- Expanded main E2E [37631363850](https://github.com/wouhliss/vgames/actions/runs/37631363850) passes real launcher parts 1 and 2, real admin and CLI/key jobs. The 8 GiB install job failed its resume assertion: a late journal mtime represented an earlier snapshot (432 MB), despite 3.60 GB of reported progress. The repair waits for verified journal bits representing at least 40 percent before SIGKILL; the original resume, byte-identical tree, disk and flipped-byte assertions stay intact. The late-fsync regression passes; full-scale validation is running.

## Integrator-loop install checkpoint repair (INS-08)
- Main E2E [37631363850](https://github.com/wouhliss/vgames/actions/runs/37631363850) failed only its 8 GiB install job. The actual release launcher passed both parts in 13m11s; [desktop matrix 37631369611](https://github.com/wouhliss/vgames/actions/runs/37631369611) passed on all three systems.
- A journal mtime measures completion of a possibly old fsync snapshot. The kill predicate now counts durable bits from the manifest-bound journal. A late-mtime regression passes.
- A full-scale probe exposed coalesced progress on restart: its first event already included one new 4 MiB chunk. The repair reads the exact checkpoint after reaping the killed process and additionally requires that the worker fetch exactly the missing chunks. Original resume thresholds, byte equality, disk ceiling, corruption refusal and lifecycle checks remain. The first-run test bandwidth is capped at 64 MiB/s to let full-scale journal flushes keep up; deadlines are unchanged. Corrected full-scale validation subsequently passed (receipts below and under Done).
- INS #119 merged while the install-only main E2E failure was being repaired. Its seventeen hosted checks passed on its earlier head; the merge did not rerun against the subsequently advanced base. The repair is rebased onto `72e35dc` and its current-base launcher checks will verify the combination. No security transport weakening is present in the new shared hook.
- The 2026-10-07 local full-scale checkpoint run was interrupted after publishing the fixture; it supplies no passing receipt. On 2026-10-10 the repair is rebased onto current main, including SafeRoot and launcher publishing, and full-scale validation is restarted.

- Checkpoint repair on the current base passes the whole launcher (331 units), PR-scale install/publishing and chat/chaos scenarios: desktop gate 3m51s; changelog/security 6s; gitleaks 1s and actionlint pass. The earlier full-scale probe confirmed the 6,195,145,912-byte durable restart and exact missing-chunk count, but produced no final receipt after allocating both 8 GiB trees. The comparison now streams exact bytes in 64 KiB blocks and adds a boundary/length/extra-path regression; the final full-scale receipt is recorded below.

- Final checkpoint source passes the complete 8 GiB/100,004-file scenario in 420.85s: killed at 5,083,655,352 reported bytes, resumed from 5,079,461,048 durable bytes, fetched exactly missing chunks, byte-identical tree, data peak 8,694,951,096 bytes (exact final size) plus 17,023,872 bytes of journal metadata. Corruption refusal, launch and uninstall all pass. Final clippy and both comparison/journal regressions pass; hosted validation passed before #168 merged (see Done).
- INT-05 session isolation: a 401 now stashes account-bound drafts, clears all cached admin data, cancels outstanding reads and clears the CSRF fallback before navigation. All 151 foundation/history units and type checking pass. The new isolation regression fails immediately with the previous remove-only-identity behavior and passes with the fix; mutation source was restored.

- INT-05 refreshed foundation on the completed INS base: full TypeScript gate passes (1m03s), including 151 admin units and 292 launcher units; all 25 foundation/history mock browser cases pass. The combined robustness stack passes 326 admin units and 34 mock browser cases. A real-mode keyboard test raced the filtered request: waiting for that exact response preserves the focus checks and passes; the corrected full real-API run passes all 22 applicable cases (twelve unchanged pre-existing mock-only exclusions).

- INT-05 stored-draft review rejects future timestamps as well as expired/malformed drafts, preserving the one-day lifetime. The updated expiry regression fails when that check is removed and passes after restoration. Full TypeScript validation passes (53s; 151 admin and 292 launcher units); all 25 mock browser cases pass (1m03s). The infrastructure fragment describes only the delivered hook/storage; form integration follows separately.

- INT-05 refreshed combined real-API suite after the future-date fix: all 22 applicable browser cases pass (1.1m), including every new robustness case and the draft re-sign-in round trip; twelve unchanged pre-existing mock-only exclusions remain. Temporary fixture keys, database and API process were removed.

- INT-05 form-integration preparation on the published foundation: the full local TypeScript gate passes (54s), with 159 admin units and 292 launcher units. Restored package/server/allowlist/compatibility inputs keep their original ETags or revisions; signing credentials remain outside drafts. The matching combined stack passes all 22 applicable real-API browser cases after the future-date fix.

- INT-05: preceding admin adoption [#169](https://github.com/wouhliss/vgames/pull/169) merged at `d81eb7b` after all applicable current-base hosted checks passed. The forms portion is now being published on `d81eb7b`; remaining portions stay unpublished until their turn.
- INT-05 long-list preparation passes the full TypeScript gate (1m01s; 163 admin and 292 launcher units), including the 10,000-row scroll/window regressions. The matching combined robustness stack passes all 34 mock cases (1m09s). Current-base publication and hosted checks remain required. The preceding merge receipt below tracks the main base used for this portion.

- INT-05: preceding admin adoption [#170](https://github.com/wouhliss/vgames/pull/170) merged at `6026c92` after all applicable current-base hosted checks passed. The lists portion is now being published on `6026c92`; remaining portions stay unpublished until their turn.

- INT-05 long-list publication #171 stopped on its hosted keyboard-filter case: the unfiltered table already contains Hollow Harbor, so focus can land on a row removed by the pending filter. A replacement preserves every assertion, waits for that exact GET response before focusing, and additionally checks focus before Enter. No rerun, timeout increase, skip or published-head rewrite is used; fresh validation is required.

- INT-05 list replacement final local checks pass: full TypeScript 55s (163 admin and 292 launcher units), all 25 mock cases 1m02s, changelog/security 5s, gitleaks 1s and actionlint. The repaired existing keyboard scenario passes against mocks (488ms) and a fresh real API (844ms); private fixture files, database and process were removed. Fresh hosted checks on the unchanged main base are required before replacing #171.

- INT-05: preceding admin adoption [#172](https://github.com/wouhliss/vgames/pull/172) merged at `0c28e87` after all applicable current-base hosted checks passed. The units portion is now being published on `0c28e87`; remaining portions stay unpublished until their turn.
## Admin robustness adoption prepared (INT-05)
- PR #92 is split into account-bound draft infrastructure, form restoration, long lists/trust guidance, response/form units and browser scenarios; its frozen phase-1 status hunk is omitted. That hunk recorded the robustness/draft/windowing suite and its original passing validation; the replacement preserves those checks.
- All 319 admin unit tests pass on the combined prepared branches, including 163 newly adopted form/response-state cases. Browser adaptation adds real fake-Discord sign-in, real-build HTTP fault responses, deterministic real job fixtures and a real logout/re-sign-in draft round trip; no new scenario is skipped in real mode.
- Nine robustness browser scenarios pass against a fresh PostgreSQL database and the cached compatible debug API in 56.5s. Initial mock draft reload required the stable seeded package fixture, and real keyboard filtering required waiting for the filtered row before tabbing. Eight other mock scenarios and the corrected draft scenario pass; repeat the whole mock/real suite on the final rebased branch before publishing.


- INT-05 combined history/robustness validation on the install-test base: all 325 admin units and type checking pass; all 34 mock browser cases pass (2.2m), and all 22 real-API browser cases pass (1.1m). The twelve pre-existing mock-only real skips are unchanged; every new history and robustness scenario ran. Real fixture keys, processes and database were removed. This is manual evidence, not two scheduled nights.

- After session cache isolation, all 326 combined admin units and type checking pass. The draft sign-out/re-sign-in browser scenario passes in mock mode (3.4s) and against the real API (1.8s); the real fixture was removed. The earlier complete 34 mock / 22 real browser run remains valid for unchanged cases; every hosted suite is required again on the published branches.

- INT-05 final prepared stack on the published foundation and current form/list sources passes the complete local TypeScript gate (1m01s; 326 admin and 292 launcher units) and all 34 mock browser cases (1m09s). The matching full real-API suite passed 22 applicable cases; no adopted robustness scenario is skipped. Final publication remains in task/PR order on each current main base.

- INT-05: preceding admin adoption [#173](https://github.com/wouhliss/vgames/pull/173) merged at `5f1578f` after all applicable current-base hosted checks passed. The browser portion is now being published on `5f1578f`; remaining portions stay unpublished until their turn.

- INT-05 final browser adoption on `5f1578f`: TypeScript passes (59s, 326 admin and 292 launcher units), changelog/security (3s), gitleaks and actionlint pass. Running mock and real Playwright concurrently in the same worktree caused six missing-trace artifact failures because both use the same results directory; that attempt is not a pass. Running the unchanged mock suite alone passes all 34 cases (1m05s). The complete real-API suite passes all 22 applicable cases (1.1m), with twelve unchanged pre-existing mock-only exclusions and no new robustness exclusion. Fixtures were removed. Hosted validation remains required.

## Release signing contract review (INT-06)
- Owner decision 2026-10-05 authorizes optional OS signing. This contract aligns security invariant 1 and both release documents; authenticated bytes, version-bound updater signatures, mandatory signed-version policy and protected release approval remain required. INS is done, PLAY/GAME have not started; the explicit owner decision resolves the signing-policy change after this integrator loop. The fork-only instructions are replaced by the hosted dry run; real update restart/version and exact player-note assertions use INS-09. No signing configuration or production code changes in this PR.
- Full current main E2E dispatched after #174: [38132572358](https://github.com/wouhliss/vgames/actions/runs/38132572358), including every adopted real-API robustness scenario. Its result remains pending; the preceding complete main run is green. INT-06 merging requires current main health and all applicable contract checks.
## Security review (INT-06 updater packaged version)
- Status and What's new now use Tauri's packaged version, matching the updater plugin. A lower config version previously checked for the new artifact but filtered its notes against Cargo's unchanged version. No transport, signature, key, capability or install guard changes; `requireSignedVersion` and artifact verification remain mandatory. The regression uses package version 0.0.1 against workspace version 0.1.0.
- On the completed admin units base `5f1578f`, full desktop checks pass (3m36s): 332 normal launcher units, PR-scale install/publishing and all four chat/chaos scenarios. Full TypeScript passes (59s), changelog/security (4s), gitleaks and actionlint pass. Mutating status back to Cargo's version makes `updater_status_uses_the_packaged_version` fail its value assertion; source restoration passes all 24 updater tests. Fresh current-base publication and all three hosted desktop OS legs remain required.


- INT-06 packaged-version preparation retains the merged checkpoint guard and in-flight queue serialization. All 24 updater tests pass, including cancellation/shutdown/missing-update exits and the lower packaged-version regression; full local and hosted validation follow on the eventual current base.

- INT-06 refreshed packaged-version preparation on `92439af`: all 24 updater tests pass under Xvfb with the final INS install, publishing and folder-handle changes; formatting is clean. Hosted current-base checks will be required before merging.

- INT-06: preceding #175 merged at `36b0bbce9fdf62096d7e68cba4d8fcfcf45a23a7` after every applicable hosted check; this version portion is based on that exact main commit. Security review and existing validation receipts above remain applicable; fresh local/current-base hosted checks are required before merge. No persistent release key or tag has been generated.

## GAME compatibility contract review (integrator loop / GAME-02)
- #178 records the owner-approved D3DMetal deferral, existing launcher-owned env denylist and `compat::prefix_dir` path, per-key catalog rollback protection and the priority-hook inspection boundary. It changes no wire field or generated name; additive for INS/PLAY and no breaking ack is required. Actual helpers were checked. Chunk sharing is clarified without relaxing inspection before the remaining install. Original head `2ffa9204f369326b1a5d11cc6994f7d6d12c5624` stays immutable; a reviewed current-base replacement carries GAME attribution and fragment.
- #177 review found a real protocol regression: 4 MiB cannot hold 100 maximum ciphertext envelopes. The review comment and GAME status require a legal maximum-page regression and a justified social cap. F4 and the unbuffered realtime-header refusal appear sound; closure awaits the current-base fix and later INT-11 review. Q4 shared auth-client adoption remains GAME-04 per its task instructions.
## Real updater round-trip preparation (INT-06)
- INS-09 is on main, so the release dry run now has a prepared part 3 scenario instead of relying on the obsolete harness-pending fallback: unchanged GitHub HTTPS URLs served by a job-only trusted CA, signed AppImage replacement, exact player-facing notes and the new version observed after restart. The fixture removes hosts/trust and private files on exit.
- Type checking, Biome, shell syntax and three fixture-server regressions pass (exact streamed bytes/receipts, refused foreign/insecure/traversal URLs, refused symlinks outside artifacts). No bundled round-trip success is claimed yet. Its packaged-version fix and current-base hosted artifact validation remain necessary. WebDriver requests are bounded, private sign-in failure frames are not captured, and generated credentials are deleted after the real-application fixture.

- INT-06 round-trip fixture preparation on `92439af` passes the full local TypeScript gate (54s), changelog/codeowners/security (4s), gitleaks (1s) and actionlint. This covers the existing launcher/admin units as well as the new harness type checking; the three Python fixture regressions pass separately. Bundled update/restart validation is still pending and is not inferred from these local checks.

- INT-06: preceding #179 merged at `384010f1c21e49d9636ae8a100ebc59a0bb28cb1` after every applicable hosted check; this fixture portion is based on that exact main commit. Security review and existing validation receipts above remain applicable; fresh local/current-base hosted checks are required before merge. No persistent release key or tag has been generated.
## Release pipeline validation prepared (INT-06)
- OS certificates are optional as decided by the owner; unsigned Windows/macOS artifacts add notices to the draft. Updater signatures and the committed-key preflight stay mandatory. Real and dry builds share setup, build and collection; a separate job verifies downloaded dry artifacts. No caches or repository secrets enter the dry workflow, and throwaway key-generator output is discarded rather than logged.
- `actionlint` passes. The committed updater key remains a placeholder, so the strict preflight currently fails; INT-08's file-delivery fallback does not authorize persistent key generation. No dry-run success is claimed. Linux installation checks files while PLAY-09's smoke flag is absent; the lower AppImage is built with the same throwaway public key and the real INS-09 updater round-trip fixture is prepared in a separate prerequisite PR.
- `release-api.yml` needs no unsigned-mode change: it uses keyless cosign and the GitHub token, with no OS certificate secret.


- INT-06 refreshed workflow preparation uses a separate hosted updater job: download the signed new/lower Linux artifacts, start the real API, drive parts 1 and 3 with the unchanged GitHub URLs over trusted HTTPS, require every-byte replacement plus the new version after restart and exact player-facing notes, and retain only public-path receipts/redacted logs. Actionlint and shell syntax pass; no bundled dry-run success is claimed until current-base hosted validation passes.

## Integrator-loop review while INT-06 checks run
- Compatibility contract #178 is adopted in #179, merged at `384010f1c21e49d9636ae8a100ebc59a0bb28cb1`, and the original is closed with that actual receipt. All applicable current-base checks passed. Updater packaged-version #176 remains merged and mutation-tested; fixture #183 supplies current-base local TypeScript/policy and four containment regressions, with bundled proof pending.
- GAME-01 replacement #182 retains #177's published head and adds the requested 16 MiB legal-inbox cap, actual maximum-page HTTP regression and fixed-metadata parse logging. Hosted validation remains required.
- Reviewed GAME #180/#181: catalog per-key persistence and production override isolation are present. An asynchronous stored read can overwrite a newer accepted cache; concurrent due checks need serialization. Archive links `b -> .`, `a -> b/../outside` pass lexical checks yet resolve outside the staging root; a scratch filesystem reproduction confirms this. Exact reviews are on [#180](https://github.com/wouhliss/vgames/pull/180#issuecomment-6108614961) and [#181](https://github.com/wouhliss/vgames/pull/181#issuecomment-6108614090). Confinement and rollback sign-off remain open until the fixes and regressions land.

- Release pipeline boundary review: dry jobs have no protected release environment, repository secrets, cache, GitHub release upload or persistent signing keys. Updater preflight remains mandatory on the committed config; known public fixture-key proof is isolated to an unmerged branch and cannot satisfy the production-key requirement. Downloaded lower and upper artifacts both verify before execution. The API release uses keyless cosign and GITHUB_TOKEN and needs no unsigned-mode change.
