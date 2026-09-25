---
audience: internal
component: server
type: added
---
Social server: friends, single-use friend codes, blocks, profile visibility, and presence (`PUT /v1/presence`, realtime `presence.set`, `presence.changed` to accepted friends only, offline 30 s after the socket goes away), plus the `social.sweep` job.
