---
audience: internal
component: server
type: fixed
---
CI: the SQLx job's `cargo install sqlx-cli` uses `--force`, so it no longer fails when the shared rust-cache restored the binaries while the sqlx-cli cache missed.
