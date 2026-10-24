---
audience: internal
component: launcher
type: added
---
Cloud-save restores run as recorded transactions: interrupted restores roll back before a package's saves are used again, failures are undone and recorded, and old backups are pruned (PLAY-06).
