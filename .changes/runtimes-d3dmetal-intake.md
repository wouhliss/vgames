---
audience: internal
component: launcher
type: added
---
`runtimes-d3dmetal.yml` (A5-T12): D3DMetal intake. A maintainer uploads Apple's Game Porting Toolkit image to the draft release `runtimes-intake` and runs the workflow; on an Apple silicon runner it requires Apple's own strict code signature on `D3DMetal.framework`, packs it unmodified with Apple's license text, re-verifies the unpacked archive, publishes it to the `runtimes` release and opens a catalog PR (macOS aarch64, non-commercial). Steps in `docs/security/release.md`.
