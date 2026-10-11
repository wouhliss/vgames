---
audience: internal
component: launcher
type: added
---
GAME-06: bounded no-panic PE import scan; INS's priority-files hook refuses D3D12 on a Mac with `d3d12_unsupported_on_mac` before other pack bytes and records the scan per release; Rosetta 2 detection and confirmed install; `CompatPlanner` builds Wine plans (DXMT → DXVK-macOS → wined3d, `d3dmetal` skipped, backend DLL overrides, Retina default); `compat-e2e.yml` gains a macos-15 Wine job.
