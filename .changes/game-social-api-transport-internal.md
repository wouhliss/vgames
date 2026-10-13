---
audience: internal
component: launcher
type: changed
---
GAME-04: social REST uses the API client's transport (`send_with_retries`, `problem_from` with the shared `api::MAX_PROBLEM_BYTES`), keeping GAME-01's no-redirect rule and the 64 KiB/16 MiB body caps; unsafe server codes are replaced by generic ones.
