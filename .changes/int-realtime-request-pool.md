---
audience: internal
component: server
type: fixed
---
Keep realtime LISTEN on a single independent connection so it cannot exhaust the request pool or the shared SQLx test connection budget (INT-04).
