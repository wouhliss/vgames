# INS status (phase 2 · Install & library)

Phase-1 record: [agent-2.md](agent-2.md) (not edited). Only what is live on `main` is carried over below (Q15).

## Done
- INS-01 release selection (`catalog::release`), this status file, generated library types in the UI (PR link in the
  next update).

## In progress

## Interfaces delivered (other agents may now rely on these)
- INS-01 `vgames_desktop_lib::catalog::release::{select_release, route_for, preferences, Route, SelectedRelease,
  host_platform, to_core}`: 09 §1 in full. `host_platform` is a re-export of `launch::orchestrate::host_platform`
  (one host detection); `Route::is_native()` is true exactly where `launch::orchestrate::runs_natively` is (tested
  for every host × build).

Live on `main` from phase 1 (Agent 2), owned by INS now:
- Engines: `vgames_transfer::install::{fetch_release, verify_release, load_local_release, install, remove_install,
  preview_uninstall, read_record, check_space}`, `vgames_transfer::download::{DownloadControl, Progress,
  PackUrlSource, DownloadOptions}`, `vgames_transfer::update::{plan, verify::verify_install,
  execute::{update_safe, repair_safe}}`, `vgames_transfer::move_install::move_install`,
  `vgames_transfer::upload::{run, publish}`; test support behind feature `testkit` (`TestPackage`, `Rig`, `MockApi`).
- `vgames_pack` planner, pack streams, `PackStreamVerifier`; `packages/pack-wasm`.
- Launcher: `state.servers` / `ApiClient` (`servers.api(id)`), trust (`trust_state`, `refresh_trust`), SQLite
  (`state.db.call`, `db::migrations::MIGRATIONS`), libraries (commands `libraries_list`, `library_pick_folder`,
  `library_add`, `library_set_default`, `library_remove`; removing the default promotes the oldest remaining one),
  storage for collections/favorites (`db::collections`) and the install queue (`db::download_jobs`), the cover cache
  (`images::ImageCache`, `vgimg://`).

## Needs from others

## Blockers / contract questions
- Closed (Q14): phase-1 Agent 2 asked Agent 1 for a "queue history migration". The launcher's SQLite is INS's own;
  INS-03 adds the history as a launcher migration in `db/migrations.rs`. Nothing is needed from the server.
- Open from phase 1, now INS-07 (Q12): a directory component can be swapped for a symlink between validation and
  file access in the install path.

## Built for you
