---
audience: internal
component: launcher
type: changed
---
Compat profiles and local overrides can no longer set `PROTON_LOG` or `PROTON_LOG_DIR`: profiles keep applying without them and overrides with them are refused (GAME-14 threat review C3).
