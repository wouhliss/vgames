---
audience: internal
component: launcher
type: added
---
A3-T06: Downloads screen (running, up next, completed) on live `install-progress` events, with pause/resume/retry/remove, cancel dialog (keep or delete partial files), reordering (menu and Alt+arrows) and a message per pause reason and failure (disk full links to Storage settings; signature and trust failures never offer a retry). Pending IPC contract in `apps/desktop/src/ipc/contract/downloads.ts` (`downloads_list`, `download_pause|resume|retry|cancel|remove`, `downloads_reorder`, `downloads_history_clear`, event `downloads-changed`) with a mock queue and a browser progress simulator.
