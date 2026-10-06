# INS status

Install & Library (phase 2, `docs/agents/phase-2/ins-install-library.md`).

## Done

## In progress
- INS-01 — Library screens on the generated library commands (pending UI entries and types deleted); next, release
  selection from PR #85 as `catalog/release.rs`.

## Interfaces delivered (other agents may now rely on these)
- Inherited from phase-1 Agent 2 and live on `main` (PR links in `agent-2.md` → Done), now owned by INS:
  - `vgames-pack` and `vgames-transfer` (install, signed-manifest update planner and commit, verify, repair, move,
    uninstall preview, upload/publish engine).
  - Library commands `libraries_list`, `library_pick_folder`, `library_add`, `library_set_default`, `library_remove`
    (refuses libraries with installs or downloads; removing the default promotes the oldest remaining library).
  - SQLite storage for collections and favorites, the install queue (`download_jobs`, atomic active claim, startup
    requeue, waiting-job reorder), the cover cache (10 MiB entries, 500 MB LRU) and the `vgimg://` protocol.

## Needs from others

## Built for you

## Blockers / contract questions
- Closed (Q14): the phase-1 request "queue history needs a new migration" (`agent-2.md` → Agent 1). The launcher's
  SQLite is INS's; INS-03 adds the history migration.
- Carried from phase 1, open: directory components can be swapped for symlinks between validation and later access
  (A2-T04 follow-up) → INS-07; `CollectionError` has no generic failure variant → INS-04.
- This session pushes through `claude/intelligent-meitner-9hvimv` (the only branch it may push) instead of
  `ins/p2-<topic>`, one PR at a time.
