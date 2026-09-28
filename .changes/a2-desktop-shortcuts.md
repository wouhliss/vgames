---
audience: internal
component: launcher
type: added
---
A2-T10: desktop shortcuts. Adds `.url`, `.desktop` and `.webloc` renderers that open `vgames://launch/<id>`, with names that are safe on every OS. Creation never overwrites an existing file. Removal only deletes regular files that still open that package. Adds the `shortcut_create` command (argument `pkg`) and records shortcuts for uninstall cleanup. The `vgames://launch` route and icon rendering are still to come.
