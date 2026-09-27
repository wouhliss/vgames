---
audience: internal
component: launcher
type: added
---
Launcher end-to-end encrypted messaging (A4-T08): device registration and one-time-key top-ups per server, an inbox that decrypts, stores and acknowledges, an outbox with retries while offline and `unknown_devices` resends, device-change notices, a changed key blocking sends until trusted, safety-number commands, typing indicators, and the messaging commands and events of 05-social-notes §5–§6. The API now relays realtime `typing` frames to the other members of a conversation.
