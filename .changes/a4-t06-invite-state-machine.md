---
audience: internal
component: server
type: added
---
Game invite server: create (friends only, published package with a release, one active invite per sender, invitee and package, 60/h), accept, decline, cancel, and invitee status reports (installing with progress published at most every 2 s, ready, joined, failed with a reason), expiry (10 min pending, 24 h after accepting) and `invite.created` / `invite.updated` to both parties.
