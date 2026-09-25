---
audience: internal
component: server
type: added
---
Uploaded versions are verified server-side pack by pack (progress and the first failing chunk are reported to admins), then admins publish them as the current release or yank them with a reason, falling back to the previous release.
