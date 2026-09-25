## What & why

<!-- One paragraph. Link the task id from docs/agents/*.md (e.g. A1-T07). -->

## How it was verified

<!-- Commands you ran and their results. Screenshots for UI. -->

## Checklist

- [ ] Stayed inside my ownership area (AGENTS.md §2), or this is a separate `contract:` PR
- [ ] Wrote a `.changes/*.md` fragment myself (`audience: user` only for changes players can see)
- [ ] Tests added/updated; `cargo test` / `pnpm test` pass locally
- [ ] No secrets, tokens, keys or signed URLs in code, logs, fixtures or screenshots
- [ ] OpenAPI / migrations / architecture docs updated if behaviour changed

## Security review (only if this PR touches a security-critical path)

Security-critical paths are the `SECURITY-CRITICAL` section of `.github/CODEOWNERS` (vgames-core, transfer,
API auth/trust/uploads/versions, launcher deep links/auth/launch/updater/capabilities/CSP, social crypto,
CI and supply chain).

- [ ] I went through [docs/security/review-checklist.md](../docs/security/review-checklist.md) and every item is either satisfied or not touched
- [ ] Every verification step I added or changed has a negative test (one test per failure)
- [ ] Every new parser of untrusted input has size limits and a no-panic property test
