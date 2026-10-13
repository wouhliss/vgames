---
audience: internal
component: launcher
type: fixed
---
The launch-session exit-hook test releases the game's child process explicitly instead of relying on a 0.4 s sleep, so a slow CI runner no longer fails it.
