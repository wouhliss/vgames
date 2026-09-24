# Rules for agents working on vgames

Read this file completely before your first change. Then read, in order:
`docs/architecture/00-overview.md` → `01-security.md` → the docs your task list names →
your task list in `docs/agents/`.

## 1. Non-negotiables

1. **Security invariants** in `docs/architecture/01-security.md` (top of file) are release
   blockers. If a task seems to require breaking one, stop and write it up under "Blockers" in your status file (§6).
2. **The architecture docs, `openapi/openapi.yaml` and the migrations are contracts.** Do not
   silently diverge. Change them first through a separate PR titled `contract: <what>`, and
   note it in your status file so the other agents see it.
3. **No secrets in the repo**: no keys, tokens, `.env` files, signed URLs or real Discord ids in
   code, tests, fixtures, logs or screenshots.
4. **Never weaken a check to make a test pass.** Fix the code or the test's assumptions.
5. Launcher WebView code never calls the network or filesystem directly. Everything goes through
   typed Tauri commands (00-overview §3.1).

## 2. Ownership (parallel work without collisions)

Each path has one owner (00-overview §5). You may **read** everything. You **write** only
inside your area. For anything else:

| You need… | Do this |
|---|---|
| A new workspace dependency | Add it to root `Cargo.toml` `[workspace.dependencies]` (add-only, never change an existing version in the same PR as feature work) |
| An API change | `contract:` PR editing `openapi/openapi.yaml` + `docs/architecture/03-api.md`; Agent 1 (or Agent 4 for social tags) implements |
| A DB change | New migration file only (never edit a merged one). Social tables: Agent 4; everything else: Agent 1 |
| A Tauri command the UI needs | Agent 3 requests it in their status file; Agent 2/4 implements and regenerates `apps/desktop/src/bindings.ts` |
| A change to `vgames-core` | Agent 5 owns it. Propose it in a `contract:` PR with tests |

## 3. Workflow

- Branch per task group: `agent<N>/<short-topic>` (e.g. `agent2/download-engine`). Small PRs
  (< ~600 changed lines excluding generated files) that each leave `main` green.
- Commits: Conventional Commits (`feat(api): …`, `fix(desktop): …`, `test(core): …`,
  `contract: …`). Reference the task id (`A2-T07`).
- **You write the changelog, not a human.** Every PR adds a `.changes/*.md` fragment written by you, the
  agent making the change (`.changes/README.md`). Use `audience: user` only for changes a player can see
  or feel, worded for players in one plain sentence; everything else is `audience: internal`. At release
  time a release-notes agent curates these fragments (08-release §3.4). It can drop or reword a user
  fragment but never promote an internal one, so label honestly.
- Rebase on `main` before opening a PR; never force-push `main`.

### How to write your fragment

Add `.changes/<short-kebab-slug>.md` in the same PR (CI's `Changelog fragments` check fails without one;
`cargo xtask changelog lint` runs the same rules locally):

```markdown
---
audience: user        # user | internal
component: launcher   # launcher | admin | server
type: fixed           # added | changed | fixed | removed | security
---
Downloads now resume after your computer restarts.
```

- `audience: user` only when a player (or an admin in the admin UI) can see or feel the change. The text is shown
  **verbatim** to players: one or two plain sentences, 10–240 characters, starting with a capital letter and ending
  with a period. No technical words (`refactor`, `dependency`, `bump`, `crate`, `CI`, `test`, `rust`, `tauri`, `API`,
  `endpoint`, `schema`, `migration`, `PR`, `commit`, … in any form), no backticks, file paths, file names, `#123`
  or hashes. `type: security` says what could have happened to the player ("Fixed an issue that could …"), never how.
- Everything else is `audience: internal` (any wording; it only reaches `CHANGELOG.md`). Label honestly: the
  release-notes agent may drop or reword user fragments but can never promote an internal one.

| Good (`audience: user`) | Bad |
|---|---|
| You can now pin favorite packages to the top of your library. | Added `pinned` column to the library table. |
| Downloads now resume after your computer restarts. | Refactored the download engine. |
| Fixed an issue that could show another server's covers in your library. | Fixed XSS via crafted cover URL (#42). |
| Installing large games no longer freezes the launcher for a few seconds. | Bumped tauri to 2.12.

## 4. Commands

```sh
# one-time
docker compose up -d                 # Postgres 18 + GCS emulator
cp .env.example .env                 # then fill what your task needs
pnpm install

# Rust (non-desktop crates; works without WebView system libraries)
cargo check                          # default members exclude the desktop crate
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all

# Desktop (needs WebKitGTK 4.1 on Linux / WebView2 on Windows)
cargo check -p vgames-desktop
pnpm dev:desktop

# API
cargo run -p vgames-api              # http://localhost:8080, Swagger UI at /docs
cargo sqlx prepare --workspace       # after changing any query! commit .sqlx/

# TypeScript
pnpm lint && pnpm typecheck && pnpm test
pnpm api:types                       # regenerate packages/api-client after openapi.yaml changes
pnpm openapi:lint

# Repo automation
cargo xtask changelog lint
```

## 5. Code standards

**Rust**
- Edition 2024, MSRV in `Cargo.toml`. Workspace lints apply (`[lints] workspace = true`).
- Libraries: typed errors with `thiserror`. Binaries: `anyhow` at the edges only.
- No `unwrap`/`expect`/`panic!`/indexing that can panic on untrusted input. In tests they are fine.
- `unsafe` only for FFI, with `#[allow(unsafe_code)]` on the smallest scope and a `// SAFETY:` comment.
- Logging via `tracing` with structured fields; never `println!` in libraries. Redact secrets.
- Async: never block the runtime (file hashing and disk writes go to blocking pools); every
  network call has a timeout; every loop that waits has a cancellation path.
- Tests live next to the code (`#[cfg(test)]`) plus `tests/` for integration. Use `insta`
  snapshots for wire formats, `proptest` for invariants, `wiremock` for HTTP.

**TypeScript / React**
- TS strict (see `tsconfig.base.json`), no `any`, no non-null assertions on data from IPC or HTTP.
- Admin web: validate every HTTP response with zod at the boundary, even though the client is generated.
  Desktop: IPC payloads are typed by tauri-specta and come from our Rust core; do not re-validate them.
- Never `dangerouslySetInnerHTML`. Render server text as plain text or through the shared safe Markdown renderer.
- Every async view handles loading, empty, error, and (admin) 401/403/409/412 states explicitly.
- Biome for lint/format. Vitest for units, Playwright for E2E.

## 6. Status files (how agents coordinate)

Keep `docs/agents/status/agent-<N>.md` current (create it on your first task):

```markdown
# Agent N status
## Done
- A<N>-T01 … (PR link)
## In progress
## Interfaces delivered (other agents may now rely on these)
- `vgames_core::manifest::Manifest::parse_and_validate(bytes) -> Result<…>` (A5-T03)
## Needs from others
- From Agent 2: command `library_list()` (for A3-T05)
## Blockers / contract questions
```

## 7. Definition of done (every task)

- Acceptance criteria in the task list are met, with tests that would fail without the change.
- `cargo fmt`, `clippy -D warnings`, tests, `pnpm lint/typecheck/test` pass.
- Docs updated when behavior or a contract changed; changelog fragment added.
- No new warnings, no TODOs without an owner and task id.
