---
audience: internal
component: server
type: fixed
---
`Cargo.lock`: `yoke-derive` 0.8.3 → 0.8.4. 0.8.3 was yanked from crates.io on 2026-09-30, which failed the "Supply chain" check (`cargo deny`, `yanked = "deny"`) on `main` and every PR. Transitive only (ICU crates under `url`/`idna`); no code change.
