---
audience: internal
component: launcher
type: added
---
Social features now follow the launcher's sign-in (A2-T07): `social::session_bridge` fills the session slot from the
active server's account and access token, refreshes a refused token once (REST calls retry once with it) and empties
the slot on sign-out. The realtime socket reconnects after close 4001 once the token is refreshed.
