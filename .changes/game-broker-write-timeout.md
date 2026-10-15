---
audience: internal
component: launcher
type: fixed
---
Overlay broker: writes to the renderer time out after 5 s and never hide the stop signal, so a renderer that stops reading cannot keep the broker task alive (GAME-14 threat review, O4).
