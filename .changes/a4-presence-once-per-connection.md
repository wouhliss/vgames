---
audience: internal
component: launcher
type: fixed
---
Social presence is sent once per realtime connection: a presence update published between the socket turning connected and the `hello` handler no longer causes a duplicate `presence.set` (connection epoch in `RealtimeHandle`).
