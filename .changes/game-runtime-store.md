---
audience: internal
component: launcher
type: added
---
GAME-03: compatibility runtimes download resumably with the pinned size enforced, are hashed before any extraction, extract path-safely (no `..`, absolute paths, outward links or device files) into a staging directory, are reference-counted per installed package and garbage-collected; runbooks §7 step 4 describes the per-key catalog version.
