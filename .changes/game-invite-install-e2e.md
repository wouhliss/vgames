---
audience: internal
component: launcher
type: added
---
GAME-12: `tests/invite_install_e2e.rs` runs a game invite on the real install path (real API with realtime, a published package, Bob's whole launcher with its download worker): accept → install requested → `installs::start` → installing with progress → ready after `InstallFinished` → the installed game launched → joined; part of the required database scenarios.
