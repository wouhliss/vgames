---
audience: internal
component: launcher
type: added
---
Scan declared save folders through directory capabilities, reject links and overlapping roots, enforce file/byte limits, and hash only metadata-changed files with cancellation. Missing native known folders scan as empty without being created; dangling base links remain refused.
