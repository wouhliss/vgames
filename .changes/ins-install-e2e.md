---
audience: internal
component: launcher
type: added
---
INS-08: `tests/install_e2e.rs` runs publish → install_start → SIGKILL at 40 % (after a journal flush) → resume → byte-identical tree → disk bound → corrupted pack refused before the chunk is written → dummy game launched → uninstall, against the real API in process. Small scale in the required desktop CI job, 8 GiB nightly.
