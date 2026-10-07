---
audience: internal
component: launcher
type: added
---
Install queue worker (`downloads/`): 1–3 concurrent jobs through `fetch_release` → `check_space` → `install`, event-driven pause and resume (offline, library offline, disk full), integrity failures reported and never retried, cancel with keep or delete, `pause_all_at_checkpoint` for the updater and the priority-files hook for GAME-06 (INS-03).
