---
audience: internal
component: launcher
type: added
---
A2-T09: Linux game process tracking. Games start in their own process group. The launcher waits on pidfds in poll with no timers until the whole group has exited, re-attaches after a restart (pid plus start time), can stop the tree, and leaves the game running when the wait is cancelled. Windows and macOS trackers are still to come.
