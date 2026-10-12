---
audience: internal
component: launcher
type: added
---
GAME-13: hosted social soak jobs in `soak.yml` (1 h chat at 2 msg/s, 5 h 30 min idle sockets, Postgres 18 service, RSS samples in the job summary and artifacts), and an explicit 2 MiB SQLite `cache_size` so the page cache does not shape the RSS curve.
