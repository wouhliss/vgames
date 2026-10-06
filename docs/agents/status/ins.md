# INS status

Install & Library (phase 2, `docs/agents/phase-2/ins-install-library.md`).

## Done
- INS-01 — Library screens on the generated library commands, pending UI entries and types deleted
  ([#113](https://github.com/wouhliss/vgames/pull/113), merged); release selection from PR #85 as
  `catalog/release.rs` (this PR; #85 closed as superseded). Evidence: a table test of every host against every set of
  available platforms (09 §1), and the native route matches `launch::orchestrate::runs_natively` for every
  host × platform pair; the acceptance grep finds no pending library call.

## In progress
- Split agreed 2026-10-06 between the two INS sessions (the user started two): session_016VNaVFJqCdjyJ8qqaTzFtN takes
  INS-02, INS-03, INS-04 and INS-08 (the install chain); session_01AChegfo4ZUhpjL2LgRbUk3 takes
  INS-05, INS-06 and INS-07; INS-09 and INS-10 go to whoever finishes first. Each keeps the other's lines here.
- INS-05 — Accounts, API client and cross-server isolation (session_01AChegfo4ZUhpjL2LgRbUk3): account commands,
  problem bodies and the auth hook in this PR; the cross-server token test next.

## Interfaces delivered (other agents may now rely on these)
- Inherited from phase-1 Agent 2 and live on `main` (PR links in `agent-2.md` → Done), now owned by INS:
  - `vgames-pack` and `vgames-transfer` (install, signed-manifest update planner and commit, verify, repair, move,
    uninstall preview, upload/publish engine).
  - Library commands `libraries_list`, `library_pick_folder`, `library_add`, `library_set_default`, `library_remove`
    (refuses libraries with installs or downloads; removing the default promotes the oldest remaining library).
  - SQLite storage for collections and favorites, the install queue (`download_jobs`, atomic active claim, startup
    requeue, waiting-job reorder), the cover cache (10 MiB entries, 500 MB LRU) and the `vgimg://` protocol.

- `catalog::release::{select_release, Route, SelectedRelease, host_platform}` (INS-01): the build this computer
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

## Built for you

## Blockers / contract questions
- Closed (Q14): the phase-1 request "queue history needs a new migration" (`agent-2.md` → Agent 1). The launcher's
  SQLite is INS's; INS-03 adds the history migration.
- Carried from phase 1, open: directory components can be swapped for symlinks between validation and later access
  (A2-T04 follow-up) → INS-07; `CollectionError` has no generic failure variant → INS-04.
- This session pushes through `claude/intelligent-meitner-9hvimv` (the only branch it may push) instead of
  `ins/p2-<topic>`, one PR at a time.
