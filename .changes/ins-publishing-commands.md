---
audience: internal
component: launcher
type: added
---
INS-06: launcher publishing in Rust (`publishing/`, `commands/publishing.rs`): admin packages and versions, folder plans that list every invalid entry, publishing jobs that check the key against the trust bundle before uploading, survive a restart and stop at `ready`, release and yank. `tests/publish_e2e.rs` runs it against the real API (2 GiB on PRs, 5 GiB nightly).
