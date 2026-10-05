---
audience: internal
component: launcher
type: fixed
---
CI is green again on Rust 1.99: the download engine's worker counter no longer uses the deprecated `AtomicUsize::fetch_update` (a `compare_exchange_weak` loop with the same semantics, unit-tested, compiling at the declared MSRV), and `rust-toolchain.toml` pins the exact toolchain so a new Rust release can no longer break `main` without a PR.
