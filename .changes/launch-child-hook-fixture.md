---
audience: internal
component: launcher
type: fixed
---
Keep the hook regression child alive through an explicit FIFO release instead of assuming launch/database work finishes before a short sleep. The early-exit and exactly-once assertions remain unchanged (PLAY-01).
