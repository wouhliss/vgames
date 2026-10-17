---
audience: internal
component: server
type: fixed
---
The realtime gateway now rate-limits every inbound frame per connection (burst 20, 10/s), drops excess frames without a reply and closes sockets that keep flooding (GAME-14 S5).
