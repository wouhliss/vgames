---
audience: internal
component: launcher
type: fixed
---
The launcher's Rust core now drops presence events from non-friends, typing events from non-members of the conversation, and over-long, blank or control-character game titles before they reach the UI or overlay (GAME-14 threat review S1, S2).
