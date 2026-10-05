# Project introspection, 2026-10-05

Snapshot of `main` at `eb5f29d` (last merge 2026-09-30; nothing merged since). This is the evidence the
phase-2 work split ([README.md](README.md)) is built from. Every claim names its proof: a file, a command
output, a CI run or a PR. Items marked **Q** are quality problems that became phase-2 tasks; items marked
**B** are blocking areas.

## 1. Verdict in five lines

1. The **server** (API, DB, storage, auth, trust, versions, saves, social relay) and the **admin web** are
   essentially complete and well tested. The **CLI** and the key pipeline work end to end, nightly.
2. The **launcher** is a polished UI on top of mostly mock data. **47 of the commands the UI calls do not
   exist in Rust** (§4, B1): you can add a server, sign in, chat and receive invites, but you cannot browse the
   catalog, install, update, verify, uninstall or (on Windows and macOS) launch anything in a real build.
3. The work is unevenly spread: **most of what is left sits in phase-1 Agent 2's area** (launcher core: 11 of its 18
   tasks are unfinished). It stalled on 2026-09-28 with one green PR never merged (#85).
4. `main` has been **red since 2026-10-02** because the Rust toolchain floats on `stable` and Rust 1.99
   deprecated one method. Every open PR, Dependabot included, fails CI until that one-line fix lands.
5. No milestone after M1 can be demonstrated yet. M2 (publish → install) is the critical path, and the
   install pipeline wiring is its only missing piece. The engines underneath it are done and tested.

## 2. Decisions taken with the owner today

| Topic | Decision | Consequence in phase 2 |
|---|---|---|
| Release keys and secrets | **Agents generate, the owner pastes.** | INT generates the updater key and the runtime-catalog key, commits the public halves, and hands the private halves to the owner through the session, never through git. The owner pastes them into the `release` environment ([human-checklist.md](human-checklist.md)). Keys are not made offline, which 01-security assumed; runbooks get a "rotate to offline keys" path. |
| OS code signing | **Ship unsigned.** | The release workflow gets an explicit unsigned mode for the canonical repo (it fails today without certificates, §5 Q9). Updater (minisign) signing stays mandatory. macOS controller emulation stays off (no CoreHID entitlement), with the in-app explanation 07 §4.1 already plans. |
| D3DMetal | **Deferred.** | macOS v1 runs D3D9–D3D11 through Wine + DXMT / DXVK-macOS. D3D12 titles are refused up front on every Mac (as on Intel Macs today). Formats keep accepting `d3dmetal` for later; PR #102 (intake workflow) is parked. |
| Hardware validation | **Hosted CI + manual checklist.** | Agents prove everything they can on GitHub-hosted Windows, macOS and Linux runners (WARP, lavapipe, llvmpipe, Xvfb). Real-GPU, exclusive-fullscreen and 24 h checks go on a non-blocking manual checklist. The soak moves to hosted runners (≤ 6 h per job). |

## 3. What is done (by area)

### 3.1 Server (phase-1 Agent 1): complete

A1-T01…T17 are all done ([status](../status/agent-1.md)): config, problem+json, pagination, idempotency, ETags,
rate limits on every route (test walks every operation), OpenAPI drift test (allowlist empty), Discord sign-in
with PKCE and refresh rotation, realtime gateway with LISTEN/NOTIFY fan-out, job queue, GCS + fs storage with a
conformance suite, trust endpoints, catalog, metadata (IGDB/Steam with SSRF guard), version upload / finalize /
verify / publish / yank, release descriptors, compat profiles, cloud saves, admin server endpoints with an
authorization matrix, admin web hosting, and measured p99 < 26 ms at 200 rps on the hot routes.

### 3.2 Admin web (phase-1 Agent 3): complete except the robustness suite

A3-T13…T17 done: packages, metadata, images, versions with a resumable browser upload (pack-wasm, key Worker),
compat profile editor, users, allowlist, settings, trust, jobs, audit. **A3-T18** (robustness suite) is written
and was green in PR #92, but never merged and is now 34 commits behind.

### 3.3 Shared crates and tooling (Agents 2 and 5): complete

`vgames-core` (formats, layout, paths with NFKC rules, signatures, key files, trust, `verify_manifest`, compat,
runtime catalog verification, no-panic property tests), `vgames-pack` (planner, pack streams, verifier, WASM),
`vgames-transfer` (download engine: 2.4–3.0 GB/s loopback, 243 MiB peak, 50 random kills resume identically;
upload engine; update planner; verify, repair, move, uninstall), `vgames-cli` (key ceremonies, trust build,
sign, publish, re-sign, `verify`, login), `xtask` (changelog lint/release, OpenAPI check, updater sign/manifest,
runtimes build/sign/verify, security check, codeowners check).

### 3.4 CI, release and security (phase-1 Agent 5): complete except what needed keys

13 CI jobs, a 3-OS desktop matrix (green nightly), nightly E2E (key pipeline green), release workflows for the
launcher (draft, updater re-signing per version, SBOM, provenance) and the API image (cosign), Dependabot,
CODEOWNERS, security test matrix, runbooks, and a full security review
([review-2026-09-30.md](../../security/review-2026-09-30.md)). Waiting on keys: PR #46 (runtime catalog
signing pipeline) and PR #102 (D3DMetal intake, now parked).

### 3.5 Social (phase-1 Agent 4): server and launcher logic complete; overlay partly done

A4-T01…T09 done: E2EE (vodozemac Olm, encrypted at rest, safety numbers), friends, presence, devices, relay,
invites, realtime client with resync, messaging and invites in the launcher, proven against the real API
in-process (`tests/social_chat.rs`, `social_chaos.rs`). Overlay: broker, protocol, hub, safety valve, hotkey,
fallback window and the **Vulkan layer on Linux** (tested on lavapipe with the validation layer) are done.
Missing: Windows DLL (hudhook) and injector, Linux GL preload, macOS NSPanel, panel input in-game, the 1 h and
24 h soaks (25 min runs recorded), and the handoff.

### 3.6 Launcher UI (phase-1 Agent 3): screens done, mostly against mocks

Onboarding, shell, library (5,000 tiles at 0 dropped frames), browse/details/install dialog, downloads,
settings (general, servers, account, storage, downloads, updates, about, privacy, overlay, compatibility),
friends/chat/invites, update banner/What's new. Missing: Settings → Controllers and Cloud saves, cloud-save
UX (A3-T08), launcher admin publishing (A3-T11), the a11y/perf pass on real bindings (A3-T12), handoff.

### 3.7 Launcher core (phase-1 Agent 2): the gap

Done: shell, local SQLite, servers/trust/sign-in (TOFU, rotation, rollback refusal), libraries (commands),
collections/favorites/queue **storage** (DB only), cover cache + `vgimg://`, deep-link parser and routing,
shortcut files, launch target resolution, launch plans, pre-launch checks with a stamp cache, game sessions
with playtime and re-attach (**Linux only**), controller mapping tables and the emulation decision.

Not started or not wired: catalog commands, install orchestration (queue worker driving
`vgames_transfer::install`), every install/update/verify/move/uninstall command, downloads commands, account
session commands, app commands (diagnostics, appearance, licenses, open URL), Windows and macOS process
tracking, cloud-save client (A2-T11), controllers runtime (SDL3 thread, ViGEm, uinput; `sdl3` is a dependency
that no code uses), admin publishing mode (A2-T13), perf hardening (A2-T14), packaging (A2-T15), Proton
(A2-T16), Wine (A2-T17), handoff (A2-T18).

## 4. Blocking areas

### B1. The launcher cannot install anything (release gate G1)

The UI's pending contract (`apps/desktop/src/ipc/contract/*.ts`) calls 54 commands. Compared with the
generated `bindings.ts`, **47 have no Rust implementation**:

```
account_session_revoke account_sessions app_diagnostics app_licenses appearance_get appearance_set
catalog_genres catalog_list collection_add_package collection_create collection_delete
collection_remove_package collection_rename collections_list collections_reorder compat_default_set
compat_licenses compat_override_reset compat_override_set compat_overview compat_packages
credential_storage download_cancel download_pause download_remove download_resume download_retry
download_settings_get download_settings_set downloads_history_clear downloads_list downloads_reorder
favorite_set install_move install_open_folder install_plan install_resume install_start install_uninstall
install_uninstall_plan install_update install_verify installs_list open_external_url package_details
rosetta_install runtime_remove
```

Plus the events `ui-nav`, `active-controller-changed`, `libraries-changed`, `installs-changed`,
`collections-changed`, `downloads-changed`. Seven pending entries (`libraries_list`, `library_*`,
`package_overlay*`) are already generated and should be deleted from the pending files. In a real build the
Library, Browse, Downloads, Storage/Downloads/Compatibility settings and the invite → install flow all fail
with "command not found". **Blocks:** M1 (empty catalog step), M2, M3 (invite → install), M3b, M4, the UI's
real-mode tests, the invite install path, and the security sign-off (G1, F1).

### B2. Games cannot be launched on Windows or macOS

`apps/desktop/src-tauri/src/launch/process/unsupported.rs` returns `ProcessError::Unsupported` on every
non-Linux OS. Windows is the primary target platform. It also blocks the overlay DLL injection, which needs
the "spawn suspended → hook → resume" step the overlay needs.

### B3. `main` is red, so nothing can merge

The CI runs `CI` (scheduled, 2026-10-02 → 10-05) fail at `clippy` in both "Rust" and "Desktop build check".
Reproduced locally with rustc 1.99.0 (released 2026-10-01):

```
error: use of deprecated method `std::sync::atomic::Atomic::<usize>::fetch_update`: renamed to `try_update`
   --> crates/vgames-transfer/src/download/fetch.rs:178:14
```

With `-A deprecated`, workspace and desktop clippy both pass with no other finding, so this is the only
breakage. Cause: `rust-toolchain.toml` says `channel = "stable"`. Every open PR fails too (Dependabot #104,
#105, #106). `try_update` does not exist at the declared MSRV (1.95), so the fix is a `compare_exchange` loop plus an exact
toolchain pin. **Fixed in the PR that introduced this plan** (`fetch.rs` loop with tests, `rust-toolchain.toml`
pinned to 1.99.0); the toolchain policy follows in INT-01.

### B4. Process bottlenecks that waited on people

- Contract PRs waited for "the orchestrator", meaning a person, to merge them. Phase 2 makes the INT agent the
  integrator, who merges them under written rules.
- Release keys waited on people. They are resolved by the decisions in §2.
- Hardware acceptance criteria could never be met in agent sessions. They are resolved by "hosted CI +
  checklist".

### B5. One agent held most of the critical path

Phase-1 Agent 2 owned the transfer engines **and** every launcher subsystem, and phase-1 Agent 3 owned every
screen, so most features waited on two agents. Phase 2 uses four vertical slices (INS, PLAY, GAME, INT), each
owning the Rust, the UI and the tests of its features, so a feature waits on nobody.

## 5. Things done poorly (now phase-2 tasks)

| Id | Problem | Evidence | Fixed by |
|---|---|---|---|
| Q1 | Toolchain floats on `stable`; a new Rust release breaks `main` with no PR involved. MSRV `1.95` is declared but never tested. | §4 B3; `rust-toolchain.toml` | This PR; INT-01 |
| Q2 | The launcher test suite is barely gated. The required Linux desktop job runs clippy plus `updater::` tests only; the full suite runs only in the non-required "Desktop matrix"; the DB-backed `social_chat`/`social_chaos` tests are `#[ignore]` and run nowhere in CI (Agent 4 asked twice). | `.github/workflows/ci.yml` lines 322–325 | INT-03 |
| Q3 | Nightly E2E red since 2026-09-27: `apps/admin-web/e2e/foundation.spec.ts` assumes the mock build (`/discord\.example/`, `?mock=admin`) while the nightly job runs against the real API. Reported by Agent 5 on 09-27, never fixed. | E2E runs 36849111278 → 37299022926 | INT-05 |
| Q4 | The social REST client (`social/api.rs`) bypasses the launcher's HTTP hardening: it **follows redirects** (the main client sets `Policy::none()` so a bearer token never follows one) and reads bodies with an **uncapped** `.json()` (the main client uses `read_capped`). A compromised server can make the launcher buffer unbounded responses. | `social/api.rs:32-37,151,164` vs `api/mod.rs:101-108` | GAME-01 |
| Q5 | Open security findings from the 2026-09-30 review: F1 (Verify must adopt a re-signed envelope), F2 (catalog version per key), F3/F4 (secrets in `Debug`), F5 (64 MiB realtime frames on the launcher; no realtime decoder no-panic test on either side), G1. Verified still open in code today. | `vgames-proto/src/auth.rs:60`, `vgames-overlay/src/protocol.rs:147`, `social/realtime.rs:414` | INS-04 (F1), GAME-03 (F2), INT-04 (F3, F5 server), GAME-01 (F4, F5 launcher), INT-11 (G1 sign-off) |
| Q6 | CODEOWNERS security section names paths that do not exist (`src-tauri/src/deeplink/`, `src-tauri/src/auth/`, `apps/api/src/social.rs`). The real files (`deeplink.rs`, `servers/auth.rs`, `secrets.rs`, `api/session.rs`, `social/store*`) are therefore **not** marked security-critical, so PRs touching them skip the security checklist. `cargo xtask codeowners check` does not detect dead patterns. | `.github/CODEOWNERS` | INT-02 |
| Q7 | Contract drift. 03-api §6 lacks server→client `typing` and `presence.changed.package_title`. 02 lacks the NFKC path rule, byte-wise file order, empty-file hash and `version_label` length that `vgames-core` enforces. 09 lacks the launcher-owned env denylist (`WINEPREFIX`, `STEAM_COMPAT_*`, …). 08-release requires OS signing (now: unsigned). 09 assumes D3DMetal (now: deferred). | grep of the docs vs `vgames-core` | INT-02, INT-06, GAME-02, GAME-14 |
| Q8 | PR hygiene. #63 and #68 are superseded (their content landed through other commits) but still open. #85, #92 and #103 are green but unmerged and now behind. #79, #80 and #81 (utoipa 6, utoipa-axum 0.3, swagger-ui 10) can only be merged together. #77 (TypeScript 7 for `packages/api-client`) contradicts the deliberate TS 5 pin there. #76 (`@types/node` 26) is ahead of the Node 22 engine. | `list_pull_requests` | INT-01, INT-04, INT-05, INS-01 |
| Q9 | The release workflow refuses unsigned builds in the canonical repo (`UNSIGNED` only on forks; macOS errors without `APPLE_SIGNING_IDENTITY`). The release-runtimes workflow would fail on merge without its secrets. Dry runs need a fork and a person. | `release-desktop.yml:158,258` | INT-06 |
| Q10 | The soak workflow targets a self-hosted runner that does not exist, so it has never run (`skipped`). The 1 h chat and 24 h idle soaks were cut to 25 min by the sandbox. | `soak.yml:30-31`; Soak run 37102930756 | INT-07, PLAY-10, GAME-13 |
| Q11 | Placeholders shipped on `main`: `plugins.updater.pubkey = "REPLACE_WITH_TAURI_UPDATER_PUBLIC_KEY"`; no `runtimes/runtime-catalog.pub`; overlay binaries and the Linux udev rule are not in `bundle.resources`; `sdl3` is compiled but unused. | `tauri.conf.json:93`; `Cargo.toml` | INT-08, PLAY-04, PLAY-09, GAME-11 |
| Q12 | Known local race: a directory component can be swapped for a symlink between validation and file access in the install path (phase-1 Agent 2's own blocker). | [status](../status/agent-2.md) "Blockers" | INS-07 |
| Q13 | Intermittent tests reported but not fixed: `vgames-transfer` `protocol_violations_are_retried_once_on_a_fresh_connection` (Linux, once in CI and once for Agent 4), the API socket-presence test's 10 s startup wait under the parallel suite. | status files of Agents 1, 4, 5 | INS-07, INT-04 |
| Q14 | Requests sent to the wrong owner sat unanswered. Agent 2 asked Agent 1 for a "queue history migration", but the launcher's SQLite is Agent 2's own. Agent 5's "contract PR to 02/09 recording clarifications" was never opened. | status files | INS-01, INT-02 |
| Q15 | Status files drifted: work merged long ago is still listed as "in progress" (Agent 2's T07 note, Agent 3's "Part 3 (this PR)", Agent 5's T11/T13). Readers cannot tell what is live. | status files | The first task of every agent |
| Q16 | No automated demo of any milestone. M1–M4 were meant to be run by the orchestrator by hand, so regressions in the integrated flow are invisible. | `docs/agents/README.md` | INS-09, INT-09 |

## 6. Test evidence gathered today

- `cargo clippy --all-targets --locked -- -D warnings` (rustc 1.99.0): fails only on Q1/B3; passes with
  `-A deprecated`. Same for `-p vgames-desktop` (WebKitGTK installed in the container).
- `cargo test --locked` (default members) with Postgres 18 from `docker compose`: **552 passed, 0 failed,
  4 ignored**. The tests compile on 1.99 because they do not run with `-D warnings`.
- `cargo test -p vgames-desktop --locked` under Xvfb: **263 passed, 0 failed, 7 ignored**. The ignored DB-backed
  tests, run with `VGAMES_TEST_DATABASE_URL`, also pass: `social_chat` 2/2 and `social_chaos` 2/2, in about
  15 s in total. They are cheap enough for the required job (Q2).
- CI history: `CI` green on main until 2026-10-01; red from 2026-10-02 (B3). `Desktop matrix` green nightly.
  `E2E`: key pipeline green, admin real-API red (Q3). `Soak`: skipped (Q10).

So what exists is solid. The problems are what is missing (B1, B2), what is not gated (Q2) and what drifted
(Q1, Q3, Q6–Q9), not broken code.

## 7. Milestone status

| Milestone | Status | Missing |
|---|---|---|
| M1 Hello server | Mostly | `catalog_list` (the "empty catalog" step) and the `ui-nav` controller events |
| M2 Publish → install | Server and CLI side yes (`vgames publish` → verify → publish → `vgames verify`, nightly) | Every launcher install command (B1), plus an automated demo |
| M3 Social | Proven in-process (`social_chat.rs` runs the invite → install → ready → join flow with a fake library) | Real install path (B1), overlay toast on a hosted runner, automated demo |
| M3b Everywhere | Not started | Process tracking on Windows and macOS (B2), Proton, Wine (no D3DMetal), cloud saves client, overlay renderers |
| M4 Release candidate | Not started | All of the above, plus the unsigned release mode, a dry run, hosted soak and budgets on release builds |
