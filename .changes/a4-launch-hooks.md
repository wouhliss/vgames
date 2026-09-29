---
audience: internal
component: launcher
type: added
---
Launcher launch hooks: the overlay adds its environment to every game launch and drops its broker when a start fails; invites launch through the real launcher (`LauncherGames`), replacing `social::ports::join_args`.
