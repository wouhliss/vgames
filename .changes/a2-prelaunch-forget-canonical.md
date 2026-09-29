---
audience: internal
component: launcher
type: fixed
---
A2-T09: `Prelaunch::forget` now also matches the canonical install path. Cached executables are keyed by it, so a root given with the Windows `\\?\` prefix missing, or through a linked folder, left a stale verified entry behind after an update.
