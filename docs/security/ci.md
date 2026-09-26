# CI and branch protection

Owner: Agent 5. Workflow: [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml). Humans configure
the GitHub settings below; agents cannot.

## What runs on every PR and push to `main`

| Check (job name) | What it enforces |
|---|---|
| `Rust (fmt, clippy, tests)` | `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo test` (default members) against a Postgres 18 service (`#[sqlx::test]` creates one database per test) |
| `SQLx offline data and migrations` | Migrations apply to an empty database; the upgrade path from the base revision works and no merged migration was edited (checksum mismatch fails); `cargo sqlx prepare --workspace --check` (committed `.sqlx/` matches the queries) |
| `Changelog fragments` | Also `cargo xtask codeowners check` (every tracked file has a code owner; security-critical paths resolve to the security section of `.github/CODEOWNERS`). PRs: `cargo xtask changelog check --base <base>` — every fragment is valid (08-release §3.2), the PR adds at least one, and a warning when `apps/desktop/**` changed without an `audience: user` launcher fragment. Pushes: `cargo xtask changelog lint` |
| `WASM (vgames-core, pack-wasm)` | `vgames-core --features wasm` builds and passes clippy on `wasm32-unknown-unknown`; the `@vgames/pack-wasm` package builds and its Node smoke test passes |
| `TypeScript (Biome, typecheck, Vitest, OpenAPI lint)` | `pnpm install --frozen-lockfile`, `pnpm lint`, `pnpm typecheck`, `pnpm test`, `redocly lint` of `openapi/openapi.yaml` |
| `Launcher UI end-to-end (mock mode)` | Playwright suite of `apps/desktop` against the mock backend (Chromium) |
| `Desktop build check (Linux, WebKitGTK)` | Builds the launcher UI, then `cargo clippy -p vgames-desktop -D warnings` with the Tauri system libraries |
| `Supply chain (cargo-deny, cargo-audit, pnpm audit)` | [`deny.toml`](../../deny.toml): RustSec advisories, license allowlist (no GPL/AGPL), crates.io only, duplicate versions reported; `cargo audit`; `pnpm audit --prod` |
| `Secret scan (gitleaks)` | Full history with [`.gitleaks.toml`](../../.gitleaks.toml): default rules plus `vga_`/`vgr_`/`vgs_` tokens, `vgames.key/1` key files, `.vgkey` files, minisign secret keys |
| `Workflows (actionlint)` | Every workflow in `.github/workflows/` passes actionlint (expression and permission checks, shellcheck of `run:` scripts); the release workflows are otherwise only exercised at release time |
| `Dependency review` | PRs only: new dependencies with moderate+ advisories or GPL/AGPL licenses. Needs GitHub Advanced Security on a private repository (see below) |

The OpenAPI drift check runs inside `cargo test` (`apps/api/tests/openapi_contract.rs`).

Other workflows: [`desktop-matrix.yml`](../../.github/workflows/desktop-matrix.yml) (Windows, Linux, macOS
builds and Rust tests of the launcher crates on PRs touching them, and nightly; not required) and
[`release-desktop.yml`](../../.github/workflows/release-desktop.yml) (see [release.md](release.md)).

Workflow rules (08-release §1): every action is pinned by commit SHA, `permissions: {}` at the top and
`contents: read` per job, no secrets in `ci.yml` (fork PRs get nothing to steal), caches keyed by lockfiles
and saved only from `main`, `concurrency` cancels superseded runs.

## Branch protection for `main` (humans)

Settings → Rules → Rulesets → new branch ruleset targeting `main`:

1. **Require a pull request before merging.** Required approvals: 0 while the agents run unattended (raise it
   to 1 when humans review), dismiss stale approvals, require review from Code Owners once
   `.github/CODEOWNERS` lands (A5-T10).
2. **Require status checks to pass**, with "Require branches to be up to date before merging". Required checks
   (exact job names):
   - `Rust (fmt, clippy, tests)`
   - `SQLx offline data and migrations`
   - `Changelog fragments`
   - `WASM (vgames-core, pack-wasm)`
   - `TypeScript (Biome, typecheck, Vitest, OpenAPI lint)`
   - `Launcher UI end-to-end (mock mode)`
   - `Desktop build check (Linux, WebKitGTK)`
   - `Supply chain (cargo-deny, cargo-audit, pnpm audit)`
   - `Secret scan (gitleaks)`
   - `Dependency review` (only after enabling it, see below; a skipped job never satisfies a required check)
3. **Require linear history** and allow **rebase merging only** (agents use `gh pr merge --rebase`).
4. **Block force pushes** and **restrict deletions**.
5. Do not add bypass actors for the agents' token. If agents cannot create PRs (token without
   `pull_requests: write`), grant that permission to their fine-grained token instead of letting them push to `main`.

Tags: protect `desktop-v*`, `api-v*` and `runtimes-*` with a tag ruleset so only maintainers can create them.

## Repository settings (humans)

- Actions → General → Workflow permissions: **Read repository contents** (the default token is read-only;
  jobs widen it explicitly). Do **not** allow Actions to create or approve pull requests, except for the
  `runtimes.yml` bot flow (A5-T12), which uses its own `pull-requests: write` job permission.
- Actions → General → Fork pull request workflows: **require approval for all outside collaborators**.
- Environments → `release`: required reviewers (humans), deployment branches limited to protected tags.
  Release secrets live only there (infra/README.md §2).
- Dependency review on a private repository needs GitHub Advanced Security (Code Security). When it is enabled,
  set the repository variable `DEPENDENCY_REVIEW=true` (Settings → Secrets and variables → Actions → Variables)
  and add `Dependency review` to the required checks.

## Running the checks locally

`scripts/ci/local.sh` runs the jobs of `ci.yml` on your machine before you push (commit first). It checks the committed
tree against its merge base with `origin/main` and writes `target/ci-local/summary.md`. `--list` names the jobs, and
`scripts/ci/local.sh rust changelog` runs a subset. A missing tool fails its job with the install command. Postgres 18
comes from `DATABASE_URL`, or from `docker compose up -d postgres`. Keep its jobs in step with `ci.yml` (both owned by
Agent 5).

## Public repository (since 2026-09-26, humans)

- Actions minutes are free on public repositories. The private-repo quota ran out on 2026-09-25, which is why jobs
  failed before starting that evening.
- Fork PRs: keep "Require approval for all outside collaborators". `ci.yml` uses no secrets, and every secret stays
  in the approval-gated `release` environment.
- Turn on **secret scanning** and **push protection** (free on public repositories; they complement gitleaks), and
  **private vulnerability reporting**.
- `Dependency review` now runs on every PR (it is free on public repositories). Add it to the required checks.
- `soak.yml` uses a self-hosted runner. It never runs on `pull_request`, so fork code cannot reach that runner; keep it
  that way, and restrict the runner group to this repository.
