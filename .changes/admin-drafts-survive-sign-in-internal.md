---
audience: internal
component: admin
type: added
---
A3-T18: form drafts (package create/editor, settings, allowlist, compatibility profiles) are stashed to sessionStorage on any 401 and restored once after sign-in, scoped to the account and carrying the ETag or revision they were edited against. The 401 hook moved into the HTTP layer so direct calls from forms reach it, not only queries.
