---
audience: internal
component: launcher
type: added
---
GAME-08: the hosted Proton end to end now registers the Vulkan overlay layer and starts a real overlay broker, and asserts the layer connects from inside the Proton game (`VGAMES_PROTON_E2E_OVERLAY`). Overlay changes now also run `compat-e2e.yml`.
