# Security and release operations

Owner: Agent 5. This page lists where each security and release document lives, and everything that still needs
a person before the first release. The rules themselves are in [01-security](../architecture/01-security.md).

## Where things are

| Document | What it covers |
|---|---|
| [ci.md](ci.md) | Every CI job, the required checks and branch protection |
| [release.md](release.md) | Keys and secrets, cutting launcher, server and runtime-catalog releases, D3DMetal intake, scheduled workflows |
| [runbooks.md](runbooks.md) | Key ceremonies, revocation (with a drill), root rotation, key compromise, a compromised server or admin account, antivirus false positives, message templates |
| [test-matrix.md](test-matrix.md) | Each invariant and threat, and the tests that fail if it breaks |
| [review-checklist.md](review-checklist.md) | The review for every security-critical PR and every release |
| [review-2026-09-30.md](review-2026-09-30.md) | The last full review, its findings and the release gate G1 |

Open findings and requests to other agents are in [Agent 5's status file](../agents/status/agent-5.md), under
"Needs from others".

## Before the first release (humans)

Each item links to its exact steps. Nothing here can be done by an agent: every item needs a person's account, a
key held offline or a repository setting.

1. **Branch protection** for `main`: the required checks listed in [ci.md](ci.md#branch-protection-for-main-humans),
   including "Admin UI end-to-end (mock mode)".
2. **The `release` environment** with human reviewers, deploying from the release tags, plus a tag ruleset for
   `desktop-v*`, `api-v*` and `runtimes-*` ([release.md](release.md#one-time-setup-humans), steps 1–2). Once the
   runtime pipeline (https://github.com/wouhliss/vgames/pull/46) is merged, also allow `main`: the runtime
   catalog is signed on merge.
3. **Updater key**: generate it offline and replace `REPLACE_WITH_TAURI_UPDATER_PUBLIC_KEY` in
   `apps/desktop/src-tauri/tauri.conf.json` with its public key; the private key and its password go into
   `release` (step 3). Without it, no launcher can update itself.
4. **Code signing**: the Windows certificate and the Apple Developer ID secrets (steps 4–5).
5. **Runtime-catalog key**: create it offline, commit `runtimes/runtime-catalog.pub`, and store the key and its
   password in `release` ([release.md](release.md#runtime-catalog)). PR 46 waits for it. Also allow GitHub
   Actions to create pull requests (Settings → Actions → General), so the runtime watcher can propose new
   versions.
6. **Release notes**: `ANTHROPIC_API_KEY` in `release` (step 6). Then do one dry run against the live API on a
   test tag and record it in Agent 5's status file (A5-T08 acceptance).
7. **Dry run on a fork** with test keys: installable artifacts, a valid `latest.json`, and signatures that
   verify ([release.md](release.md#dry-run-on-a-fork-a5-t07-acceptance), A5-T07 acceptance).
8. **Each vgames server**: a root key ceremony and the first trust bundle ([runbooks.md](runbooks.md) §1),
   then a publisher key per admin (§2).
9. **Each Game Porting Toolkit release**: the D3DMetal intake, which needs an Apple ID to download the Toolkit
   ([release.md](release.md#runtime-catalog), once the intake workflow is merged after PR 46).
10. **Optional**: the `runtimes-bot` GitHub App (CI on the watcher's pull requests), and the self-hosted runner
    for the weekly soak test (`VGAMES_SOAK_RUNNER=true`, [release.md](release.md#scheduled-workflows)).

And, from the last review: before shipping, the launcher's install, update and verify commands (Agent 2,
A2-T08) must exist and pass the review items listed under G1 in
[review-2026-09-30.md](review-2026-09-30.md).
