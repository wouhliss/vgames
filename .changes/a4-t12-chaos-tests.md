---
audience: internal
component: server
type: fixed
---
Realtime: a lost `LISTEN` connection is now detected (`try_recv`) and followed by `close_all(1012)` so clients resync; chaos tests for two API instances, an API restart and a database connection reset (A4-T12).
