---
audience: internal
component: launcher
type: added
---
A2-T07: desktop `api` client (problem+json → typed errors, idempotent-only retries, single-flight refresh with rotation, 401 → one refresh + one retry), `servers` hub (TOFU root pin, persistent fingerprint-mismatch block, trust bundle refresh with rollback refusal and root rotation), PKCE sign-in with deep-link and paste-code paths, keychain token vault with a 0600 file fallback, strict `vgames://server/add` and `auth/callback` routing, and migration `0003_servers_trust`.
