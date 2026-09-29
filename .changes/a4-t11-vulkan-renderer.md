---
audience: internal
component: launcher
type: added
---
In-game overlay Vulkan layer (`vgames-overlay`, feature `renderer`): toasts and the panel are drawn into presented frames by a buffer-to-image copy, with a lavapipe end-to-end test under the Khronos validation layer in CI (A4-T11).
