---
audience: internal
component: server
type: added
---
Admin server API: user search, role changes (owner only, the last owner is protected), disabling users (sessions revoked and sockets closed at once), registration allowlist, owner-editable server settings with If-Match, job list and retry, and a filterable audit log. Asset uploads that are not multipart now get a problem+json 415 instead of plain text.
