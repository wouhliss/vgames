---
audience: internal
component: launcher
type: added
---
A4-T08: launcher messaging over the E2EE core (device registration, one-time key top-up, outbox with
backoff, inbox drain, device notices, safety-number commands, 13 new Tauri commands and 5 events).
Fallback key ids are now prefixed with `F` so they never collide with one-time key ids on the server.
The server fans out the realtime `typing` event to the other conversation members.
