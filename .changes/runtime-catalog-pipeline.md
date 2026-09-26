---
audience: internal
component: launcher
type: added
---
Runtime catalog pipeline (A5-T12, part 2): `runtimes.yml` watches the upstream Proton, umu, Wine, DXMT, DXVK-macOS and
MoltenVK releases, verifies each download (size, GitHub digest, upstream checksum, licenses), smoke-tests it (D3D11
under Proton and Wine) and keeps one update PR; `release-runtimes.yml` re-verifies new entries, signs the catalog in
the `release` environment and publishes it. `cargo xtask runtimes upsert`, and `tar` archives in the catalog format.
