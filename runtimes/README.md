# Compatibility runtime catalog

`catalog.toml` pins every compatibility runtime the launcher may download (UMU-Proton, GE-Proton,
umu-launcher, WineHQ macOS builds, D3DMetal, DXMT, DXVK-macOS, MoltenVK): URL, SHA-256, size, license.
D3DMetal is redistributed because vgames is non-commercial; it ships unmodified with Apple's license, and
the `commercial = false` flag in `catalog.toml` must stay false for it to build.
`runtimes.yml` proposes updates as PRs; `release-runtimes.yml` turns the merged catalog into the
signed `runtimes.json` the launcher trusts. Owner: Agent 5. Spec: docs/architecture/09-compatibility.md §5.
