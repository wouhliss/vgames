---
audience: internal
component: server
type: added
---
`docs/security/test-matrix.md` maps every security invariant, every threat of 01-security §1 and every attack named in A5-T11 to the automated tests that cover it. New `cargo xtask security check` (in CI): the launcher CSP and capabilities stay strict (no inline scripts, IPC-only connections, no plugin permissions, asset protocol off) and every job that reads a signing secret or mints OIDC tokens runs in the approval-gated `release` environment, with no `pull_request_target`. `ci.yml` now also runs nightly and gains an "Admin UI end-to-end (mock mode)" job, so the admin authorization matrix runs in CI.
