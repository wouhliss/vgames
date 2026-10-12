---
audience: internal
component: launcher
type: added
---
GAME-08: native 64-bit Linux launches with the overlay on get `LD_PRELOAD` pointing at the installed overlay library (one `vulkan_layer::installed_library()` lookup shared with the Vulkan layer manifest); Proton/Wine and other builds are left to the Vulkan layer, and a path `LD_PRELOAD` would split is refused.
