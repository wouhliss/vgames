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

## In progress
- Split agreed 2026-10-06 between the two INS sessions (the user started two): session_016VNaVFJqCdjyJ8qqaTzFtN takes
  INS-02, INS-03, INS-04 and INS-08 (the install chain); session_01AChegfo4ZUhpjL2LgRbUk3 (this PR's author) takes
  INS-05, INS-06 and INS-07; INS-09 and INS-10 go to whoever finishes first. Each keeps the other's lines here.
- INS-05 — Accounts, API client and cross-server isolation (next, session_01AChegfo4ZUhpjL2LgRbUk3).
- INS-04 (session_016VNaVF…) — collections and favorites done ([#123](https://github.com/wouhliss/vgames/pull/123)); `installs_list` (this PR), then
  updates/repairs/resume with the F1 re-signed-envelope adoption, then move/uninstall/open-folder, update detection
  and `libraries-changed`.

## Interfaces delivered (other agents may now rely on these)
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

## Needs from others

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

- From INT (review of #138): updater checkpoint acquisition now serializes with an in-flight SQLite claim, so it cannot report idle before that claim is registered. Added `checkpoint_waits_for_an_in_flight_database_claim`; the scoped updater guard releases the queue even when the install future is cancelled. Keep these changes when continuing INS-08.
