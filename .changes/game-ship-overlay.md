---
audience: internal
component: launcher
type: added
---
GAME-11: the overlay renderers are bundle resources (`overlay/` in Tauri's resource directory); the launcher resolves the library there for the Vulkan layer manifest and the GL preload, and the desktop matrix stages the renderer and checks it lands where the launcher looks.
