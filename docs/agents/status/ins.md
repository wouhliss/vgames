# INS status

Install & Library (phase 2, `docs/agents/phase-2/ins-install-library.md`).

## Done
- INS-03 updater checkpoint: INT reviewed and adopted [#138](https://github.com/wouhliss/vgames/pull/138) in [#144](https://github.com/wouhliss/vgames/pull/144), merged at `6069524` with cancellation cleanup and in-flight claim coverage; all CI and three OS builds green.
- INS-01 — Library screens on the generated library commands, pending UI entries and types deleted
  ([#113](https://github.com/wouhliss/vgames/pull/113), merged); release selection from PR #85 as
  `catalog/release.rs` (this PR; #85 closed as superseded). Evidence: a table test of every host against every set of
  available platforms (09 §1), and the native route matches `launch::orchestrate::runs_natively` for every
  host × platform pair; the acceptance grep finds no pending library call.
- INS-02 — catalog, genres, package details, install plans and covers ([#126](https://github.com/wouhliss/vgames/pull/126)); D3D12 blocker renamed to
  `d3d12_unsupported_on_mac` ([#110](https://github.com/wouhliss/vgames/pull/110)). Evidence: 12 wiremock tests (paging, cache, 401 refresh, offline,
  hidden package, blockers, cover cap and type sniffing, no token on signed URLs); the grep finds no
  `needs_apple_silicon`.
- INS-03 — priority files first ([#114](https://github.com/wouhliss/vgames/pull/114)), queue history and concurrent claims ([#117](https://github.com/wouhliss/vgames/pull/117)), queue worker
  ([#133](https://github.com/wouhliss/vgames/pull/133)), download commands, settings and `install_start` ([#136](https://github.com/wouhliss/vgames/pull/136)). Evidence:
  `downloads/tests.rs` (install end to end, restart resume, pause/cancel mid-download, concurrency, offline
  library, out of space, refused signature, updater checkpoint, priority order).
- INS-04 — collections and favorites ([#123](https://github.com/wouhliss/vgames/pull/123)), `installs_list` ([#139](https://github.com/wouhliss/vgames/pull/139)), updates, repairs and
  resumed installs with the F1 re-signed-envelope adoption ([#140](https://github.com/wouhliss/vgames/pull/140)), move/uninstall/open-folder, update
  detection without polling and `libraries-changed` ([#141](https://github.com/wouhliss/vgames/pull/141)). Evidence: `downloads/tests.rs` (an update of 1
  file in 10,000 fetches only its chunk; verify repairs a damaged file and a clean verify downloads nothing; F1:
  revoked key → launch refused → verify adopts the re-signed envelope with no pack byte fetched; uninstall with and
  without leftovers never follows a link; a moved install still verifies; every `InstallActionError`), detection
  against wiremock; no pending entry left in `contract/library.ts` (file deleted) nor `librariesChanged` in `core.ts`.

- INS-08 — `tests/install_e2e.rs` ([#146](https://github.com/wouhliss/vgames/pull/146)): the real API in process,
  publish → `install_start` → SIGKILL at 40 % after a journal flush → resume → byte-identical tree → data never above
  the final size → a flipped pack byte refused as `damaged_file` before its chunk is written → dummy game launched →
  uninstall. Small scale in the required `desktop` job (`scripts/ci/desktop-db-tests.sh`); 8 GiB nightly: first run
  green, [E2E run 37604975806](https://github.com/wouhliss/vgames/actions/runs/37604975806) (job `install`, 7 min).
- INS-09 — real-application E2E (#153, #150): `apps/desktop/e2e-real/` drives the **release** launcher through
  tauri-driver and WebKitWebDriver under Xvfb against the real API (Postgres, fs storage, fake Discord) behind TLS
  on `https://localhost`, each instance isolated by `HOME`, XDG dirs and its own D-Bus session. Part 1 (M1: add
  server, fingerprint, sign in, library folder) and part 2 (M2: publish with the CLI, browse, install, progress,
  play, stop, verify, uninstall). Nightly in `e2e.yml` (job `launcher`); first CI run green,
  [E2E run 37616978222](https://github.com/wouhliss/vgames/actions/runs/37616978222) on the merged head (parts 1 and 2 in 28 s after a
  7 min release build). Bugs it found, fixed with tests: startup panic outside Tokio (#150), a first trust bundle
  never fetched before refusing an install, Browse not refreshing after a publish (#153).

## In progress
- Split agreed 2026-10-06 between the two INS sessions (the user started two): session_016VNaVFJqCdjyJ8qqaTzFtN takes
  INS-02, INS-03, INS-04 and INS-08 (the install chain); session_01AChegfo4ZUhpjL2LgRbUk3 takes
  INS-05, INS-06 and INS-07; INS-09 and INS-10 go to whoever finishes first. Each keeps the other's lines here.
- INS-05 — Accounts, API client and cross-server isolation: account commands, problem bodies and the auth hook in
  #119 (written by session_01AChegfo4ZUhpjL2LgRbUk3; that session ended on its usage limit on 2026-10-06, so
  session_016VNaVF… took over INS-05–07 on 2026-10-07 and brought #119 up to date with `main`); the cross-server
  token test `servers/tests.rs::a_request_to_one_server_never_carries_another_servers_token` in the follow-up PR
  (test-matrix row filled; noted in `int.md`).
- INS-06 — Launcher admin publishing (session_016VNaVF…), done: contract and mock
  ([#158](https://github.com/wouhliss/vgames/pull/158)); Rust commands, `vgames-transfer`
  `PublishOptions::release` and per-pack bytes, `tests/publish_e2e.rs`
  ([#160](https://github.com/wouhliss/vgames/pull/160), which carried the closed #159); the publish screen
  ([#162](https://github.com/wouhliss/vgames/pull/162)). Evidence: `publish_e2e` (wrong passphrase, untrusted key,
  cancel → restart → resume, failed verification, release, yank), `PublishPage.test.tsx` (the four cases, route
  absent for players), `e2e/publish.spec.ts` (axe).
- INS-07 (session_016VNaVF…) — flaky download test (Q13): already fixed at its root cause by `7e111a0`
  (2026-09-26). The old test queued all three protocol faults on one rig, so the single-chunk retry on a fresh
  connection that the first fault triggers (`Scheduler::mismatch`) could receive the second fault; two mismatches on
  one chunk is an integrity failure by design (02 §7.10). Both failure reports (Agents 4 and 5, 2026-09-25) predate
  the fix; the old shape fails 4/40 locally, the current test passed 600/600 under CPU contention (debug and
  release, pinned and unpinned). Acceptance loop: `.github/workflows/transfer-loop.yml` (200 consecutive
  iterations, debug and release, `ubuntu-24.04`): 200/200 in each,
  [run 37646614344](https://github.com/wouhliss/vgames/actions/runs/37646614344) ([#161](https://github.com/wouhliss/vgames/pull/161)).
- INS-07 — local race (Q12), part 1: `SafeRoot` holds the install folder as a directory handle (`cap-std`) and
  resolves every component relative to it without following links or junctions; install writes, flushes,
  finalization and update staging open files only through it (`fsutil::Target`). Tests swap a checked folder for a
  symlink (a junction on Windows) and prove no open, create, write or delete reaches outside. Part 2: update
  commit/verify, move and uninstall. Then the release-build budgets.
- INS-10 (session_016VNaVF…, partial) — `apps/desktop/README.md` sections Installs, Downloads and, under Logs,
  where install and download logs go and how to read an integrity report; the G1 re-review request for INT-11 in
  `int.md`. No INS-02/03/04 pending entry is left (`downloads.ts` and `library.ts` deleted, `catalog.ts` holds only
  GAME's `rosetta_install`). The status file is made final once INS-05–07 land.

## Interfaces delivered (other agents may now rely on these)
- INS-06: generated commands `publish_packages`, `publish_package_create`, `publish_versions`, `publish_pick_folder`,
  `publish_pick_key`, `publish_plan`, `publish_start`, `publish_jobs`, `publish_cancel`, `publish_resume`,
  `publish_dismiss`, `publish_release`, `version_yank` and event `publish-progress` (admins and owners only, checked
  in Rust); `src/mocks/publishing.ts` with fixture triggers for the four failure cases.
- `vgames_transfer::upload::publish::PublishOptions::release` (false stops at `ready`, `PublishPhase::Ready`) and
  `UploadProgress::packs` (INS-06), for the CLI too.
- Inherited from phase-1 Agent 2 and live on `main` (PR links in `agent-2.md` → Done), now owned by INS:
  - `vgames-pack` and `vgames-transfer` (install, signed-manifest update planner and commit, verify, repair, move,
    uninstall preview, upload/publish engine).
  - Library commands `libraries_list`, `library_pick_folder`, `library_add`, `library_set_default`, `library_remove`
    (refuses libraries with installs or downloads; removing the default promotes the oldest remaining library).
  - SQLite storage for collections and favorites, the install queue (`download_jobs`, atomic active claim, startup
    requeue, waiting-job reorder), the cover cache (10 MiB entries, 500 MB LRU) and the `vgimg://` protocol.

- `catalog::release::{select_release, route_for, Route, SelectedRelease, host_platform}` (INS-01; `route_for` from
  INS-02): the build this computer
  installs from a package's catalog releases and how it runs (`Route::is_native()` for native and OS emulation,
  `Proton`, `Wine { needs_rosetta }`); `host_platform` is the launch path's own.

- `ApiClient::with_auth(method, |http, token| request)` (INS-05): the "401 → refresh once → retry once" hook for any
  authenticated request (the builder sets URL, query, body and the bearer header); a request still refused after
  one refresh is `ApiError::Unauthenticated`. For GAME-01 to move `social/api.rs` off its own client.
- `ApiError::problem()` / `ApiError::status()` (INS-05): the parsed problem body of an error response (title, detail,
  up to 20 field errors, `Retry-After` seconds).
- Commands `account_sessions(server_id)`, `account_session_revoke(server_id, session_id)` (INS-05); revoking this
  launcher's own session forgets it locally (`Servers::forget_account`).

## Needs from others
- From INT (integrator sweep, 2026-10-07): #119 has green checks on its original base but has been inactive for a day. Please rebase onto current main, regenerate append-only bindings as needed, rerun every hosted check and self-merge under phase-2 README §3. The shared auth hook remains useful for GAME’s social client.

## Built for you
- For GAME: `compat::prefix_dir(data_dir, &PackageRef) -> PathBuf` = `<data_dir>/prefixes/<server_id>/<package_id>`
  on every OS ([#125](https://github.com/wouhliss/vgames/pull/125)); INS uses it for `has_prefix` and prefix removal on uninstall.

## Blockers / contract questions
- Closed (Q14): the phase-1 request "queue history needs a new migration" (`agent-2.md` → Agent 1). The launcher's
  SQLite is INS's; INS-03 adds the history migration.
- Carried from phase 1, open: directory components can be swapped for symlinks between validation and later access
  (A2-T04 follow-up) → INS-07; `CollectionError` has no generic failure variant → INS-04.
- This session pushes through `claude/intelligent-meitner-9hvimv` (the only branch it may push) instead of
  `ins/p2-<topic>`, one PR at a time.
- #119 was squash-merged by mistake (§3 says rebase merge): `main` has it as the single commit `72e35dc`, with the
  same content as the PR head. Not rewritten, since `main` is never force-pushed; later INS PRs use rebase merge.

- From INT (review of #138): updater checkpoint acquisition now serializes with an in-flight SQLite claim, so it cannot report idle before that claim is registered. Added `checkpoint_waits_for_an_in_flight_database_claim`; the scoped updater guard releases the queue even when the install future is cancelled. Keep these changes when continuing INS-08.
