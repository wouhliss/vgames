# Phase 2: finishing vgames with four independent agents

Phase 1 (2026-09-24 → 09-30) built the server, the admin web, the shared crates, CI and the launcher UI.
Phase 2 finishes the launcher core, compatibility, cloud saves, controllers, the overlay and the release, and
proves every milestone automatically. **Start with the evidence:** [introspection-2026-10-05.md](introspection-2026-10-05.md).

A seven-agent draft of this split, written the same day, was replaced by these four files before any agent started.
Task ids such as `A6-T01` or `A7-T05` in "(was …)" notes point into it:
`git log --diff-filter=D -- 'docs/agents/phase-2/agent-*.md'` shows the commit that removed it.

Four agents work in parallel. Each owns a **vertical slice**: the Rust commands, the UI screens, the tests and
the docs of its features, end to end. Paste **everything below the `---` line** of an agent's file into a fresh
agent session at the repository root.

| Agent | File | Slice (what it delivers, end to end) |
|---|---|---|
| **INS** · Install & library | [ins-install-library.md](ins-install-library.md) | Catalog, install/update/verify/move/uninstall, downloads, libraries, collections, accounts, launcher admin publishing, the transfer engines, and their screens. **Critical path** (M2). |
| **PLAY** · Launch & platform | [play-launch-platform.md](play-launch-platform.md) | Launching and tracking games on Windows/Linux/macOS, the launch hook API, controllers, cloud saves, deep links and shortcuts, app commands, packaging, launcher performance, and their screens. |
| **GAME** · In-game & social | [game-ingame-social.md](game-ingame-social.md) | Proton/Wine compatibility runtimes, the in-game overlay on every OS, social security fixes, invites on the real install path, social soak, and their screens. |
| **INT** · Integrator | [int-integrator.md](int-integrator.md) | `main` health, CI, security sign-off, release pipeline and keys, the server follow-ups, the admin web, milestone automation, and the launcher-wide UI quality gate. Merges `contract:` PRs. |

## 1. Independence: how the slices avoid waiting on each other

The split follows the dependency graph, not the technology. Every feature is owned with its UI, so the old
"frontend agent" bottleneck is gone, and each provider ships, up front, what its consumers will need:

| Consumer needs | Provided by | Designed-out how |
|---|---|---|
| Release selection for the catalog | INS (owns it, PR #85) | Lives in INS's catalog, not in compat |
| Launch hooks (saves pull/push, overlay injection, Proton/Wine plans) | PLAY-01, first task | PLAY ships the whole hook API before anything else |
| Windows "spawn suspended → inject → resume" | PLAY-02, second task | Built in, not requested |
| Install events for invites, checkpoint pause for the updater, "launch target first" download option for the D3D12 check | INS-03 | Built in the worker task, not requested |
| `cloud_saves` and `compat` fields in the library and catalog | PLAY-06 / GAME-07 | The fields are required in the contract, so INS ships a default (`unsupported`; `untested` with no blockers) behind a small provider trait (`installs/` for saves, `catalog/` for compat). PLAY-06 and GAME-07 register their providers; INS's commands never change for it |
| Prefix path of a package (uninstall, save bases) | GAME-02 | The pure function `compat::prefix_dir(data_dir: &Path, package: &events::PackageRef) -> PathBuf`, giving `<data_dir>/prefixes/<server_id>/<package_id>` on every OS; `compat::save_base` (GAME-05) maps a 06 §1 save base inside it. Whoever needs either first writes it with this signature |
| Runtime-catalog public key | INT-08 | GAME tests with the vectors and a test key until then; never blocks |

**Need it, build it.** If a task needs something from another slice and it is not on `main` when you get there:

1. Check that agent's status file. If it is about to land, do the other parts of your task first.
2. Otherwise **build the smallest version yourself**, in their area, in its own small PR (title with your task id),
   following the documented contract and keeping their tests green. Write it under "Built for you" in their status
   file. The owner extends it later; they never rewrite it from scratch.
3. Never wait and never sit idle. Tasks that consume another slice's work are ordered as late as each list allows
   (PLAY-06 is the exception: its fallbacks are spelled out because saves sit on PLAY's own hooks).

## 2. Decisions that bind every agent (owner, 2026-10-05)

1. **Keys:** INT generates the updater and runtime-catalog keys (INT-08); the owner pastes the private halves into
   GitHub. Never commit, log or paste a private key or password into a PR, issue or status file.
2. **Unsigned builds:** no Authenticode, no Apple Developer ID, no CoreHID entitlement. Updater (minisign)
   signatures stay mandatory.
3. **D3DMetal deferred:** macOS runs D3D9–11 through Wine + DXMT / DXVK-macOS; D3D12 is refused up front on every
   Mac. Formats keep accepting `d3dmetal`; no D3DMetal code path or catalog entry ships.
4. **Validation on hosted CI:** GitHub-hosted Windows, macOS and Linux runners with software rendering (WARP,
   lavapipe, llvmpipe, Xvfb). What only real hardware can show goes to [human-checklist.md](human-checklist.md) §B
   and **never blocks** a task.

## 3. Autonomy rules (you have full permissions)

Every agent is authorized to create branches, push, open PRs and **merge its own PRs** (rebase merge, never squash)
once every check is green; re-run a failed job **once** when the failure is clearly infrastructure; install system
packages in its sandbox (it runs as root); start Docker and Postgres; create issues and labels; and edit the shared
files of §5.

**Security paths are the exception to self-merge:** a PR that changes the CSP, capabilities or the updater block of
`tauri.conf.json`, or `apps/desktop/src-tauri/src/updater/**`, is not merged by its author; write
"Ready for INT: #<PR>" in your status file and continue with other work.

**INT** additionally merges `contract:` PRs (§4), merges any green PR whose author wrote "Ready for INT: #<PR>" in
their status file, closes superseded PRs with a comment naming the commits that superseded them, and makes the
smallest fix anywhere to turn a red `main` green (with a note in the owner's status file).

Nobody may weaken a test or a check to get green, skip/disable/quarantine a test, break a security invariant
(01-security, top), force-push or push directly to `main` (always a PR), or put secret material anywhere in git.
Anything only a person can do goes to [human-checklist.md](human-checklist.md) in the same PR as its automated
substitute.

## 4. Contracts

`contract: …` PRs still change `docs/architecture/**`, `openapi/openapi.yaml`, shared migrations and cross-language
command/event surfaces. **Additive** ones (new endpoint, field, command, event, section) are merged by INT as soon as
CI is green. **Breaking** ones list the affected agents; each writes `contract ack: #<PR>` in its status file or
objects there; INT merges when all acks are in, or decides after one working loop. Never block your implementation
on a contract merge: build behind the proposed contract on your branch.

The launcher UI's pending contract (`apps/desktop/src/ipc/contract/*.ts`) and mocks (`apps/desktop/src/mocks/*`)
are owned **per entry, not per file** (`core.ts`, `settings.ts` and `catalog.ts` mix slices). Each entry is edited by
**the agent that implements the command**: it adds the pending entry and fixture first, implements
the Rust, regenerates `bindings.ts`, then deletes the pending entry and switches the screen to the generated types.

## 5. Shared files (any agent; append or keep tests green)

| File(s) | Rule |
|---|---|
| `src-tauri/src/commands/mod.rs`, `commands/names.rs`, `capabilities/main.json` | Append your commands/events; the existing tests keep them in sync |
| `src-tauri/src/lib.rs`, `src-tauri/src/state.rs` | One `pub mod` + one init call / one field per service |
| `src-tauri/src/db/migrations.rs` | Append a `Migration`; on a numbering collision after rebase, **renumber yours** |
| `src-tauri/Cargo.toml`, root `[workspace.dependencies]` | Add-only; never change an existing version in a feature PR |
| `apps/desktop/src/bindings.ts` | Generated. On conflict take `origin/main`'s copy and regenerate: `VGAMES_UPDATE_BINDINGS=1 cargo test -p vgames-desktop bindings` |
| `apps/desktop/src/{app,components,nav,i18n,ipc,mocks,styles}`, `apps/desktop/e2e/**` | Shared UI foundation: change what your screens need; the whole Vitest and Playwright suites must stay green (INT-10 guards quality) |
| `apps/desktop/src/i18n/en.ts` | Append strings under your feature's key prefix |
| `src-tauri/src/events.rs` (`AppEvent`), `src-tauri/src/error.rs` (PLAY's) | Append a variant or a field; never rename or remove one |
| `src/routes/settings/SettingsPage.tsx` | One entry per section, appended |
| `src-tauri/src/launch/plan.rs` (PLAY's) | GAME edits the `ProtonPlan`/`WinePlan` fields only |
| `.github/workflows/**` (INT's) | A slice may add its own job or step under "need it, build it" (noted in `int.md`); INT consolidates in INT-03 |
| `docs/security/test-matrix.md` (INT's) | Add the row for a test you wrote |

`src` means `apps/desktop/src`, `src-tauri` means `apps/desktop/src-tauri`. The ownership map in
[00-overview §5](../../architecture/00-overview.md) matches this README; `.github/CODEOWNERS` follows in INT-02.

## 6. Task ids, branches, status files

- Task ids: `INS-01…`, `PLAY-01…`, `GAME-01…`, `INT-01…`; reference them in commits (`feat(desktop): … (INS-03)`).
- Branches: `ins/<topic>`, `play/<topic>`, `game/<topic>`, `int/<topic>`, one per PR, from `origin/main`.
- Status files: `docs/agents/status/{ins,play,game,int}.md` (format of `AGENTS.md` §6, plus a "Built for you"
  section). Phase-1 files (`agent-1.md` … `agent-5.md`) stay as the record; do not edit them.
- Every phase-1 task that was unfinished maps to a phase-2 task:

| Phase-1 task (status on 2026-10-05) | Phase-2 task |
|---|---|
| A2-T08 libraries, catalog, install orchestration (storage only) | INS-01 … INS-04 |
| A2-T09 launch and tracking (Linux only) | PLAY-01 … PLAY-03 |
| A2-T10 deep links and shortcuts (parser and files only) | PLAY-07 |
| A2-T11 cloud-save client; A3-T08 cloud-save UX; A3-T07 Settings → Cloud saves | PLAY-06 |
| A2-T12 controllers (mapping only); A3-T07 Settings → Controllers | PLAY-04, PLAY-05 |
| A2-T13 launcher admin publishing; A3-T11 its screen | INS-06 |
| A2-T14 performance | INS-07 (downloads), PLAY-10 (app) |
| A2-T15 packaging | PLAY-09 |
| A2-T16 Proton, A2-T17 Wine | GAME-02 … GAME-07 |
| A2-T18, A3-T19, A4-T13 handoffs | INS-10 and PLAY-11, INT-12, GAME-14 |
| A3-T12 accessibility and performance pass | INT-10 |
| A3-T18 admin robustness suite (PR #92) | INT-05 |
| A4-T10 macOS panel; A4-T11 renderers; A4-T12 soak | GAME-10; GAME-08, GAME-09, GAME-11; GAME-13 |
| A5-T07 dry run; A5-T08 live dry run; A5-T12 runtime pipeline (PR #46) | INT-06; INT-12; INT-08 |
| Security findings G1, F1–F5 and introspection Q1–Q16 | See the "Fixed by" column of the introspection §5 |

## 7. Waves and critical path

```
This PR: main green again (toolchain pinned, deprecated call replaced)
**INS-03 install worker ─▶ INS-04 library commands ─▶ INS-08 M2 test ─▶ INS-09 real-app E2E ─▶ INT-11 sign-off ─▶ INT-12 RC**
PLAY-01 hooks v2 ─▶ PLAY-02 Windows tracking ─▶ GAME-09 Windows overlay DLL
                 └▶ PLAY-06 cloud saves          GAME-05 Proton, GAME-06 Wine (plug into PLAY-01)
INT-08 catalog key + PR #46 ─▶ GAME-03 runtime manager (tests run on vectors before that)
```

| Wave | INS | PLAY | GAME | INT |
|---|---|---|---|---|
| **1** (start together) | 01–03 | 01–04 | 01–04 | 01–03 |
| **2** | 04–06 | 05–08 | 05–09 | 04–08 |
| **3** | 07–10 | 09–11 | 10–14 | 09–12 |

## 8. Milestones (automated)

A milestone is reached when its job is green on `main`; INT-09 collects them in one place.

| Milestone | Proved by | Gate |
|---|---|---|
| **M1 · Hello server** | INS-09 real-app E2E, part 1 (Linux, Xvfb, fake Discord) | INS-02, PLAY-04 (`ui-nav`) |
| **M2 · Publish → install** | INS-08 (in-process, kill/resume, flipped byte) + INS-09 part 2 (real app) | INS-03, INS-04 |
| **M3 · Social** | GAME-12 (real install path) + its INS-09 scenario (two instances with separate `HOME`/XDG directories, API on `https://localhost` with a throwaway CA: release builds ignore `VGAMES_PROFILE` and refuse plain http, and no flag may relax that) + GAME-08 (overlay toast in a GL/Vulkan app) | GAME-12 |
| **M3b · Everywhere** | Desktop matrix: Windows D3D11 test app with overlay (GAME-09), Linux D3D11 via Proton (GAME-05), macOS D3D11 via Wine + DXMT (GAME-06, if the runner has Metal), cloud-save round trip across the three runners in sequence (PLAY-06; server state passed between jobs as a `pg_dump` plus fs-storage artifact) | PLAY-02/03, GAME-05/06/09 |
| **M4 · Release candidate** | `release-dry-run.yml` (INT-06), hosted soak (INT-07), budgets on release builds (INS-07, PLAY-10), security sign-off (INT-11) | everything |

## 9. Environment notes for cloud sessions (checked 2026-10-05)

- Rust comes from `rust-toolchain.toml` (pinned; rustup installs it); Node 22 and pnpm 10 are installed.
- WebKitGTK is not preinstalled. For any `vgames-desktop` build:
  `apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libdbus-1-dev libudev-dev pkg-config build-essential`
- Docker is installed but its daemon is not running: `dockerd > /tmp/dockerd.log 2>&1 &`, then
  `docker compose up -d postgres`. API tests: `DATABASE_URL=postgres://vgames:vgames-dev-only@localhost:5432/vgames`;
  launcher real-API tests: `VGAMES_TEST_DATABASE_URL=postgres://vgames:vgames-dev-only@localhost:5432/postgres`.
- `xvfb-run -a` for anything that opens a window.
- The container is reclaimed when idle: commit and push at least once per task. Long soaks belong in `soak.yml`.
- GitHub: `gh` when `gh auth status` succeeds, otherwise the GitHub MCP tools (`create_pull_request`,
  `merge_pull_request`, `pull_request_read`, `actions_list`, `get_job_logs`).

## 10. Definition of done for the project

- Every task in the four files is done, with its acceptance evidence in the status file.
- `main` is green on every workflow, including nightly E2E, the desktop matrix and the hosted soak.
- M1–M4 jobs are green on `main`.
- No pending entry remains in `apps/desktop/src/ipc/contract/`: everything the UI calls exists in Rust.
- `docs/security/review-<date>.md` signs off G1 and F1–F5 with no open finding above "low".
- [human-checklist.md](human-checklist.md) §A lists only what the owner still has to paste or click.
