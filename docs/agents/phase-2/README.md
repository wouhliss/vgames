# Phase 2: finishing vgames

Phase 1 (2026-09-24 → 09-30) built the server, the admin web, the shared crates, CI and the launcher UI.
Phase 2 finishes the launcher core, the overlay, compatibility, cloud saves and the release, and proves every
milestone automatically. **Start with the evidence:** [introspection-2026-10-05.md](introspection-2026-10-05.md).

Seven agents work in parallel. Each has one file. Paste **everything below the `---` line** of that file into a
fresh agent session at the repository root.

| Agent | File | Owns (write access) in phase 2 |
|---|---|---|
| 1 · Backend & cloud saves | [agent-1-backend-saves.md](agent-1-backend-saves.md) | `apps/api/**` (not `social/`), `vgames-proto` (not social), non-social migrations, OpenAPI non-social tags, **`apps/desktop/src-tauri/src/saves/**`** (new) |
| 2 · Install pipeline | [agent-2-install-pipeline.md](agent-2-install-pipeline.md) | `src-tauri/src/{api,servers,libraries,images,installs,catalog,downloads,publishing,db}/**`, their `commands/*.rs`, `vgames-pack`, `vgames-transfer`, `packages/pack-wasm` |
| 3 · Frontend | [agent-3-frontend.md](agent-3-frontend.md) | `apps/desktop/src/**` (not `overlay/`, not `bindings.ts`), `apps/admin-web/**`, `packages/api-client/**`, `apps/desktop/e2e-real/**` (new) |
| 4 · Social & overlay | [agent-4-social-overlay.md](agent-4-social-overlay.md) | unchanged: `apps/api/src/social/**`, social migrations/proto/OpenAPI tags, `src-tauri/src/{social,overlay}/**`, `apps/desktop/src/overlay/**`, `crates/vgames-overlay/**` |
| 5 · Integrator, CI, security, release | [agent-5-integrator-release.md](agent-5-integrator-release.md) | unchanged (`vgames-core`, `vgames-cli`, `xtask`, `.github/**`, `updater/`, `runtimes/**`, `scripts/**` except `scripts/demo` shared, `deny.toml`, security docs) **plus the integrator role** (below) |
| 6 · Launcher platform (new) | [agent-6-launcher-platform.md](agent-6-launcher-platform.md) | `src-tauri/src/{launch,controllers,shortcuts,deeplink,logging,paths,events,error,state}` and their commands, `src-tauri/{build.rs,tauri.conf.json,resources/,icons/}`, `apps/desktop/perf/**`, packaging |
| 7 · Compatibility runtimes (new) | [agent-7-compat-runtimes.md](agent-7-compat-runtimes.md) | `src-tauri/src/compat/**` (Proton, Wine, runtime manager, compat profiles) and its commands, `crates/vgames-testapps/**` (new, shared with Agent 4) |

`src-tauri` means `apps/desktop/src-tauri`. The ownership map in
[00-overview §5](../../architecture/00-overview.md) is updated to match this table.

## 1. Decisions that bind every agent (owner, 2026-10-05)

1. **Keys:** agents generate the updater and runtime-catalog keys (A5-T19); the owner pastes the private halves
   into GitHub. Never commit, log or paste a private key or password into a PR, issue or status file.
2. **Unsigned builds:** no Authenticode, no Apple Developer ID, no CoreHID entitlement. Updater (minisign)
   signatures stay mandatory.
3. **D3DMetal deferred:** macOS runs D3D9–11 through Wine + DXMT / DXVK-macOS; D3D12 is refused up front on
   every Mac. Keep the formats able to describe D3DMetal; ship no D3DMetal code path or catalog entry.
4. **Validation on hosted CI:** GitHub-hosted Windows, macOS and Linux runners with software rendering (WARP,
   lavapipe, llvmpipe, Xvfb). What only real hardware can show goes to [human-checklist.md](human-checklist.md)
   §B and **never blocks** a task: write the automated part, add the manual step, move on.

## 2. Autonomy rules (you have full permissions)

The owner wants no step to wait on a person. Every agent is authorized to:

- create branches, push, open PRs and **merge its own PRs** (rebase merge, never squash) once every check on the
  PR is green; re-run a failed job **once** when the failure is clearly infrastructure (runner lost, network);
- install system packages in its sandbox (it runs as root: `apt-get install …`), start Docker, run Postgres;
- create issues and labels; comment on PRs; close its own superseded PRs;
- edit the **shared registration points** listed in §4 without asking.

Additionally, **Agent 5 (integrator)** may merge any agent's green PR whose author marked it "ready for
integrator" in their status file, merge `contract:` PRs (rules in §3), close superseded PRs of any agent with a
comment naming the commits that superseded them, and make the smallest possible fix in any area to turn a red
`main` green (with a note in that owner's status file).

What nobody may do: weaken a test or a check to get green, skip/disable/quarantine a test, break a security
invariant (01-security, top), force-push `main`, push directly to `main` (always a PR), or put secret material
anywhere in git.

**Never wait.** If you need another agent's interface, code against the written contract with a fake, record it
under "Needs from others", and take your next task. If you need a person (a secret, hardware), implement the
automated substitute, add a line to [human-checklist.md](human-checklist.md) in the same PR, and move on.

## 3. Contracts in phase 2

Contracts are still `contract: …` PRs that change `docs/architecture/**`, `openapi/openapi.yaml`, a shared
migration or a cross-language command/event surface. Phase 1 made them wait for a person; now:

- **Additive** (new endpoint, field, command, event, section): Agent 5 merges it as soon as CI is green.
- **Breaking** (rename, removal, semantic change): the PR lists the affected owners. Each writes
  `contract ack: #<PR>` under "Blockers / contract questions" in their status file, or objects there. Agent 5
  merges once all acks are in, or decides after one working loop without an answer.
- Never block implementation on a contract merge: build behind the proposed contract on your branch.
- A provider (the agent implementing commands) may add or delete **its own** pending entries in
  `apps/desktop/src/ipc/contract/*.ts` and the matching mock fixtures in `apps/desktop/src/mocks/*`, so the UI can
  build against the shape before the Rust lands. Agent 3 still owns those files otherwise.

## 4. Shared files in the launcher (append-only, anyone)

Three launcher agents (2, 6, 7) and Agents 1 and 4 all register commands. These files are **append-only**
registration points that any of them may edit (add your lines only, keep the grouping, never reorder others'):

| File | Rule |
|---|---|
| `src-tauri/src/commands/mod.rs`, `commands/names.rs`, `capabilities/main.json` | Add your commands/events; the existing tests check they stay in sync |
| `src-tauri/src/lib.rs` | `pub mod` + one init call in `setup()` |
| `src-tauri/src/state.rs` | Add a field for your service |
| `src-tauri/src/db/migrations.rs` | Append a `Migration`; if a rebase collides on the number, **renumber yours** |
| `src-tauri/Cargo.toml`, root `Cargo.toml` `[workspace.dependencies]` | Add-only; never change an existing version in a feature PR |
| `apps/desktop/src/bindings.ts` | Generated. On conflict take `origin/main`'s copy and regenerate: `VGAMES_UPDATE_BINDINGS=1 cargo test -p vgames-desktop bindings` |

## 5. Task ids

Tasks keep each agent's numbering (A1-T18 follows A1-T17). New agents start at A6-T01 and A7-T01. Unfinished
phase-1 tasks were folded into phase-2 tasks:

| Phase-1 task | Status on 2026-10-05 | Phase-2 task(s) |
|---|---|---|
| A2-T08 libraries, catalog, install orchestration | storage done, commands/worker missing | A2-T20, A2-T21, A2-T22 (+ PR #85 → A7-T01) |
| A2-T09 launch and tracking | Linux done | A6-T02 (Windows), A6-T03 (macOS), A6-T01 (hooks) |
| A2-T10 deep links, shortcuts | parser/routing/files done | A6-T06 |
| A2-T11 cloud saves client | not started | A1-T21, A1-T22 |
| A2-T12 controllers | mapping/decision done | A6-T04, A6-T05 |
| A2-T13 admin publishing (launcher) | not started | A2-T24 |
| A2-T14 performance | idle measured on debug | A2-T25 (downloads), A6-T09 (app) |
| A2-T15 packaging | not started | A6-T08 |
| A2-T16 Proton, A2-T17 Wine | not started | A7-T03 … A7-T07 |
| A2-T18 handoff | not started | A2-T27, A6-T10, A7-T08 |
| A3-T07 settings (controllers, cloud saves) | other sections done | A3-T22 |
| A3-T08, A3-T11, A3-T12, A3-T18, A3-T19 | not started / PR #92 unmerged | A3-T22, A3-T24, A3-T26, A3-T20, A3-T27 |
| A4-T10 (macOS panel), A4-T11, A4-T12, A4-T13 | partial | A4-T17, A4-T15/T16/T19, A4-T20, A4-T21 |
| A5-T07 dry run, A5-T08 live dry run, A5-T12 part 2 | waiting on people | A5-T17, A5-T19 (+ human checklist) |

## 6. Waves and critical path

```
Day 0  A5-T14 unbreak main ─────────────────────────────────────────────────────── (every merge waits on it)
       **A2-T21 install worker ─▶ A2-T22 library commands ─▶ A2-T26 M2 test ─▶ A3-T23 real-app E2E ─▶ A5-T21 sign-off ─▶ A5-T22 RC**
          ▲ A7-T02 availability API ─▶ A2-T20 catalog
       A6-T01 launch hooks v2 ─▶ A6-T02 Windows tracking ─▶ A4-T15 Windows overlay DLL
                              ├▶ A1-T22 cloud-save client (pull/push around launches)
                              └▶ A7-T05 Proton / A7-T06 Wine plans
       A5-T19 catalog key + PR #46 ─▶ A7-T03 runtime manager ─▶ A7-T05 / A7-T06
       A1-T21 saves contract, A6-T04 controllers contract ─▶ A3-T22 settings + cloud-save UX
```

| Wave | A1 | A2 | A3 | A4 | A5 | A6 | A7 |
|---|---|---|---|---|---|---|---|
| **1** (start together) | T18–T21 | T19–T21 | T20–T22 (mocks) | T14, T16 | **T14 first**, T15–T16 | T01–T03 | T01–T03 |
| **2** | T22 | T22–T24 | T23–T25 | T15, T17–T18 | T17–T19 | T04–T07 | T04–T07 |
| **3** | T23 | T25–T27 | T26–T27 | T19–T21 | T20–T22 | T08–T10 | T08 |

## 7. Milestones (automated in phase 2)

The orchestrator no longer runs demos by hand. Each milestone is a script or test that runs in CI
(A5-T20 collects them); a milestone is reached when its job is green on `main`.

| Milestone | Automated by | Gate |
|---|---|---|
| **M1 · Hello server** | A3-T23 real-app E2E, part 1 (Linux, Xvfb, fake Discord) | A2-T20, A6-T04 (`ui-nav`) |
| **M2 · Publish → install** | A2-T26 (in-process, kill/resume, flipped byte) + A3-T23 part 2 (real app) | A2-T21, A2-T22 |
| **M3 · Social** | A4-T18 (real install path) + A3-T23 part 3 (two profiles) + A4-T16 (overlay toast in a Vulkan/GL app) | A4-T18 |
| **M3b · Everywhere** | Desktop matrix: Windows D3D11 test app (WARP) with overlay (A4-T15); Linux D3D11 via Proton (A7-T05); macOS D3D11 via Wine + DXMT (A7-T06, if the runner has Metal); cloud-save round trip across the three runners (A1-T22) | A6-T02/T03, A7-T05/T06, A4-T15 |
| **M4 · Release candidate** | `release-dry-run.yml` (A5-T17), hosted soak (A5-T18), budgets on release builds (A2-T25, A6-T09), security sign-off (A5-T21) | everything |

Real hardware, a 24 h soak and the first public release are owner steps in [human-checklist.md](human-checklist.md);
they follow M4 and do not gate it.

## 8. Environment notes for cloud sessions (checked 2026-10-05)

- Rust comes from `rust-toolchain.toml` (rustup installs it); Node 22 and pnpm 10 are installed.
- WebKitGTK is not preinstalled. For any `vgames-desktop` build:
  `apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libdbus-1-dev libudev-dev pkg-config build-essential`
- Docker is installed but its daemon is not running: `dockerd > /tmp/dockerd.log 2>&1 &`, then
  `docker compose up -d postgres`. URLs: API tests `DATABASE_URL=postgres://vgames:vgames-dev-only@localhost:5432/vgames`;
  launcher real-API tests `VGAMES_TEST_DATABASE_URL=postgres://vgames:vgames-dev-only@localhost:5432/postgres`.
- `xvfb-run -a` for anything that opens a window.
- The container is reclaimed when idle: commit and push at least once per task. Long soaks belong in
  `soak.yml`, not in your session.
- GitHub: use `gh` when `gh auth status` succeeds, otherwise the GitHub MCP tools (`create_pull_request`,
  `merge_pull_request`, `pull_request_read`, `actions_list`, `get_job_logs`). Never push to `main` directly.

## 9. Status files

Keep using `docs/agents/status/agent-<N>.md` (format in `AGENTS.md` §6). On your first phase-2 session, add a
`## Phase 2` section at the top with the same subsections, and move everything that is finished under
`## Phase 1 (closed 2026-10-05)`. Agents 6 and 7 create `agent-6.md` and `agent-7.md`. Add a line
"Ready for integrator: #<PR>" when you want Agent 5 to merge a PR for you.

## 10. Definition of done for the project

- Every task in the seven files is done, with its acceptance evidence in the status file.
- `main` is green on every workflow, including nightly E2E, the desktop matrix and the hosted soak.
- M1–M4 jobs are green on `main`.
- No pending entry remains in `apps/desktop/src/ipc/contract/` (everything the UI calls exists in Rust).
- `docs/security/review-<date>.md` signs off G1 and F1–F5 with no open finding above "low".
- [human-checklist.md](human-checklist.md) §A lists only what the owner still has to paste or click, each with exact steps.
