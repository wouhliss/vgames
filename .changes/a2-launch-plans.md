---
audience: internal
component: launcher
type: added
---
A2-T09: launch module. Resolves manifest launch targets inside the install (no symlink escape), substitutes validated invite join secrets as whole arguments, and builds Native, Proton (umu-run) and Wine commands with an allowlisted host environment, manifest and compat-profile variables, and runner-owned variables that later injection cannot replace.
