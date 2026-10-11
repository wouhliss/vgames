---
audience: internal
component: launcher
type: added
---
GAME-03: the launcher fetches and verifies the signed runtime catalog under the compiled-in key (debug-only URL and key overrides), keeps the highest version per key id (F2) with its verified bytes, refreshes at most every 6 h on startup and server switch, and adds `vgames_core::runtimes::key_id`.
