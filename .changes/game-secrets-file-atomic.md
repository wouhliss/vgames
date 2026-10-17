---
audience: internal
component: launcher
type: fixed
---
The social secrets fallback file is now written atomically via a synced temp file and refused on read when it is a symlink or readable by other users (GAME-14 S4).
