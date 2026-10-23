---
audience: internal
component: launcher
type: fixed
---
Opening the launcher database retries for up to 5 s while the file is busy, so reopening it right after the last handle dropped (a restart, or a restart test on Windows) no longer fails with "database is locked".
