---
audience: internal
component: admin
type: added
---
`vgames login`/`logout` (PKCE, pasted code, root fingerprint pinned, tokens in the OS keychain with a 0600 file fallback), `vgames trust publish` (bundle verified locally before upload) and `vgames trust re-sign` (only digests the old key provably signed are re-signed with the new key), tested against a mock API and smoke-tested against the real one.
