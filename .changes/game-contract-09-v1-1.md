---
audience: internal
component: launcher
type: changed
---
GAME-02: 09-compatibility v1.1 defers D3DMetal (D3D12 refused on every Mac as `d3d12_unsupported_on_mac`), lists the launcher-owned env keys, keeps the runtime-catalog version per key (F2), detects D3D before the full download and puts prefixes under `AppPaths::data_dir` on every OS.
