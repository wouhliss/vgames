---
audience: internal
component: launcher
type: added
---
Runtime catalog (A5-T12, part 1): `vgames_core::runtimes` parses and verifies the minisign-signed `vgames.runtimes/1`
catalog (exact bytes, prehashed signatures only, rollback refused, GitHub release URLs only, D3DMetal non-commercial
tripwire), with test vectors for the launcher's runtime manager; `cargo xtask runtimes build | sign | verify` and
`runtimes/catalog.toml`.
