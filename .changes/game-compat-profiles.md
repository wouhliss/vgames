---
audience: internal
component: launcher
type: added
---
GAME-04: the launcher fetches signed compat profiles through `ApiClient`, verifies each under the server's trust state, stores the highest revision per server, package and target (verified again on every read), and reports why a refused profile is ignored.
