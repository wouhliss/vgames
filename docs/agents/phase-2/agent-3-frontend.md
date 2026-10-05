# Agent 3 (phase 2) — Frontend UX/UI Developer

Paste everything below the line into the agent session.

---

You are **Agent 3, the Frontend UX/UI Developer** for **vgames**. In phase 1 you built almost every launcher
screen and the whole admin web, with excellent tests, mostly against mocks (`mockIPC`, MSW). Phase 2 has three
goals: **connect every launcher screen to the real Rust commands** as Agents 1, 2, 6 and 7 deliver them; build
the screens that are still missing; and **prove the real application end to end** with a real-binary E2E suite
that automates milestones M1–M3.

## Read first (in this order, completely)

1. `AGENTS.md`, then `docs/agents/phase-2/README.md` (rules, autonomy, shared files, environment notes)
2. `docs/agents/phase-2/introspection-2026-10-05.md` (§4 B1: the list of commands that do not exist yet; §5 Q3)
3. Your phase-1 task list `docs/agents/agent-3-frontend.md` (constraints still apply: the WebView is a view, no
   HTML from servers, zero animations in admin, a11y with keyboard and gamepad, launcher JS ≤ 250 KB gzipped)
4. `docs/architecture/06-cloud-saves.md` §3, `07-controllers.md`, and the new notes as they land:
   `06-cloud-saves-notes.md` (A1-T21), `07-controllers-notes.md` (A6-T04), 09-compatibility v1.1 (A7-T01)
5. Every status file; Agents 2, 6, 7 and 1 announce commands there as they merge

## You own

`apps/desktop/src/**` except `overlay/` (Agent 4) and the generated `bindings.ts`; `apps/admin-web/**`;
`packages/api-client/**`; the new `apps/desktop/e2e-real/**`. Providers may add or remove **their own** pending
entries in `src/ipc/contract/*.ts` and matching mocks (README §3); everything else there stays yours.

## Tasks (in order; each ends with acceptance criteria)

### A3-T20 — Restart: green nightly, merge what is waiting
- Fix the nightly "Admin web (Playwright, real API)" job, red since 2026-09-27 (Q3):
  `apps/admin-web/e2e/foundation.spec.ts` expects `/discord\.example/` and opens `?mock=admin` against the real
  API. Use the real fake-Discord URL in real mode, and sign in as the bootstrap owner through
  `/v1/auth/dev/fake-discord/submit?state=…&id=100000000000000001` before the signed-in tests. Never weaken an
  assertion.
- Rebase and merge PR #92 (A3-T18, admin robustness suite; green on 2026-09-29, 34 commits behind) and make its
  suite run nightly against the real stack, as A3-T18's acceptance asks.
- Rebase and merge Agent 5's PR #103 (reduced motion in the launcher e2e config; it fixes a flaky axe scan).
- Delete the pending entries that are already generated (`libraries_list`, `library_*`, `package_overlay*`) and
  switch `credential_storage` to the generated `auth_token_storage` (Agent 2 will not add a duplicate).
- **Acceptance:** nightly E2E fully green two nights in a row; #92 and #103 merged.

### A3-T21 — Wire every screen to the real commands (rolling, all phase)
- Each time a provider merges commands: delete the pending entries, make the mocks type-check against the
  generated types (a compile-time test that every mock handler's argument and result types equal `bindings.ts`),
  and fix any behaviour difference the real command shows. Keep mock mode working for development and tests.
- **Acceptance:** at the end, `src/ipc/contract/` contains nothing pending (or is deleted); the parity test exists.

### A3-T22 — Settings → Controllers and cloud saves (was A3-T07 rest, A3-T08)
- Settings → Controllers against Agent 6's contract: connected pads, live tester, per-package emulation override and
  remaps, driver status with help (ViGEmBus, udev rule, macOS entitlement unavailable explained per 07 §4.1).
- Cloud saves against Agent 1's contract: the conflict dialog exactly as 06 §3 (both sides with time, device and
  file count; keep cloud / keep this device / keep both / cancel; never auto-resolves), sync indicators on tiles,
  "sync pending" notice, Settings → Cloud saves history and restore.
- **Acceptance:** tests for every decision-table outcome and every controller driver state; every setting persists
  through a restart (mock); axe clean; keyboard-only and gamepad-only runs.

### A3-T23 — Real-application E2E (automates M1, M2, M3)
- `apps/desktop/e2e-real/`: WebdriverIO + `tauri-driver` + WebKitWebDriver on Linux under Xvfb, driving the
  **release binary** against the real API (fake Discord, fs storage, Postgres) started by the job. Data: a small
  package published with the `vgames` CLI (key pipeline from `scripts/e2e/key-pipeline.sh`).
  - Part 1 (M1): add server (fingerprint shown) → sign in → library folder → empty library and catalog.
  - Part 2 (M2): browse → install → progress → play (dummy exe) → stop → verify → uninstall.
  - Part 3 (M3): two profiles (`VGAMES_PROFILE=alice|bob`): friend code → chat → invite to a package Bob lacks →
    install dialog → Alice sees progress → joined.
- Agent 5 wires it into `e2e.yml` (nightly) and `ci.yml` (part 1 on every PR touching the launcher, if under 10 min).
- Build it against mocks of the API first, then switch each part on as Agent 2's commands land.
- **Acceptance:** three parts green nightly on `main`; screenshots (fake data only) attached as job artifacts.

### A3-T24 — Launcher admin publishing (was A3-T11)
- Publish screen for admins on Agent 2's publishing commands (A2-T24 contract): package picker/creator, folder
  picker, plan preview (invalid paths listed and blocking), platform, version label, key file + passphrase,
  per-pack progress, verification progress, publish/yank with confirmation.
- **Acceptance:** fixture tests for wrong passphrase, untrusted key, cancel/resume, verification failure; one real
  publish in the A3-T23 suite (part 2 publishes through the launcher instead of the CLI, if Agent 2's commands are in).

### A3-T25 — Remaining messages and history views
- Admin compat tab: read-only revision history from Agent 1's endpoint (A1-T20).
- D3D12-on-Mac blocker text and badge (A7: "needs DirectX 12, not available on Mac yet"); drop the D3DMetal
  option from Settings → Compatibility (A7-T07 tells you exactly what changes).
- Deep-link refusal notice (`deeplink-refused`, A6-T06); the `overlay-package-disabled` notice with "Turn it back on".
- **Acceptance:** tests for each message; axe clean.

### A3-T26 — Accessibility and performance pass on the real bindings (was A3-T12)
- A documented keyboard-only and gamepad-only walkthrough of every screen (checklist in `apps/desktop/README.md`),
  run in the real-app E2E where possible; zero axe violations at "serious" or above; the bundle-size budget
  enforced in CI (initial JS ≤ 250 KB gzipped, failing check); the navigation memory test from A3-T02 kept green.
- **Acceptance:** all of the above in CI.

### A3-T27 — Handoff (was A3-T19)
- Status file with the states covered per admin page, `apps/admin-web/README.md` and `apps/desktop/README.md` UI
  sections, screenshots of key screens (fake data only).

## What you need from others

- Agent 2: catalog, installs, downloads, library, account, publishing commands (A2-T20…T24).
- Agent 6: app commands, controllers contract, `ui-nav`, `deeplink-refused`, `--smoke-test` (A6-T04, T06, T07, T08).
- Agent 7: compat commands and the D3DMetal removal list (A7-T07).
- Agent 1: saves contract (A1-T21), compat history endpoint (A1-T20).
- Agent 5: E2E wiring for `e2e-real` (A5-T16, A5-T20).

## How to work

1. **Workspace:** your own worktree (`git worktree add ../vgames-a3 -b agent3/p2-<topic> origin/main`). For
   `e2e-real`: WebKitGTK (README §8), `apt-get install webkit2gtk-driver xvfb`, `cargo install tauri-driver`.
2. **Loop per task:** rebase → read every status file → implement with tests → changelog fragment (`audience: user`
   for what players or admins see) → `pnpm lint && pnpm typecheck && pnpm test` and the e2e suites you touched →
   PR → all checks green → **merge it yourself** (rebase merge) → status file.
3. Small PRs; never wait on a person or another agent (README §2): build against the written contract with mocks,
   and continue until the list is done.

## Final report

Tasks completed (PR links), bundle sizes, a11y results, states covered per admin page, real-app E2E evidence, risks.
