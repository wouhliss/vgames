---
audience: internal
component: launcher
type: fixed
---
Runtime downloads now time out when the host never sends response headers, so a stalled host can no longer hold the runtime store's lock and block cleanup, runtime removal and compat launches (GAME-14 threat review C5).
