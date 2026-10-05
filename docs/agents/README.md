# Agent orchestration

> **Phase 2 started on 2026-10-05.** Seven agents finish the project; start at
> [phase-2/README.md](phase-2/README.md) and the evidence in
> [phase-2/introspection-2026-10-05.md](phase-2/introspection-2026-10-05.md). Everything below describes
> phase 1 and stays as the record that phase-2 tasks refer to.

Five specialized agents build vgames in parallel. Each has one prompt file. Copy
**everything below the `---` line** of that file into a fresh agent session running at
the repository root (ideally its own git worktree).

| Agent | File | Owns (write access) |
|---|---|---|
| 1 · Backend & DB | [agent-1-backend.md](agent-1-backend.md) | `apps/api/**` (not `social/`), `vgames-proto` (not social), non-social migrations, OpenAPI non-social tags |
| 2 · Tauri & Systems | [agent-2-tauri-systems.md](agent-2-tauri-systems.md) | `apps/desktop/src-tauri/**` (not `social/`, `overlay/`, `updater/`), `vgames-pack`, `vgames-transfer`, `packages/pack-wasm` |
| 3 · Frontend UX/UI | [agent-3-frontend.md](agent-3-frontend.md) | `apps/desktop/src/**` (not `overlay/`), `apps/admin-web/**`, `packages/api-client` |
| 4 · Networking & Multiplayer | [agent-4-networking.md](agent-4-networking.md) | `apps/api/src/social/**`, social migrations and proto, `src-tauri/src/{social,overlay}/**`, `apps/desktop/src/overlay/**`, `crates/vgames-overlay` |
| 5 · DevOps & Security | [agent-5-devops-security.md](agent-5-devops-security.md) | `vgames-core`, `vgames-cli`, `xtask`, `.github/**`, `src-tauri/src/updater/**`, `runtimes/**`, `scripts/release-notes/**`, `deny.toml`, security docs |

## How to run them

1. Make the initial commit and push it (the agents branch from `origin/main`).
2. Start five agent sessions, ideally one terminal per agent. Paste each prompt from **below the `---` line**.
   Each agent creates its own worktree (`../vgames-a<N>`, branch `agent<N>/work`) on its first step, so
   they never touch each other's files. Start them all at once: every prompt says what to do while
   waiting on other agents.

Agents merge small PRs into `main` continuously and rebase often. They coordinate through
`docs/agents/status/agent-<N>.md` (format in `AGENTS.md` §6). The orchestrator (you) reads
those files daily, unblocks "Needs from others", and approves `contract:` PRs.

## Dependency graph (critical path in bold)

```
A5-T01 CI ─────────────────────────────────────────────────────────────▶ (all PRs gated)
**A5-T02 core formats + layout ─▶ A5-T03 signatures/keyfiles ─▶ A5-T04 trust + verify_manifest**
        │                              │                               │
        ▼                              ▼                               ▼
**A2-T03 packer ─▶ A2-T04 download engine ─▶ A2-T05 upload ─▶ A2-T08 installs ─▶ A2-T09 launch**
        │                                          ▲
        ▼                                          │ fs-storage protocol
A1-T01..T05 skeleton/auth/realtime ─▶ A1-T07 storage ─▶ **A1-T11 versions/finalize ─▶ A1-T12 verify/publish/download**
        │                                                              │
        ▼                                                              ▼
A4-T03..T06 server social ─▶ A4-T07.. launcher social ─▶ A4-T09 invites ─▶ A4-T10/T11 overlay (needs A2-T09 launch plan)
A5-T04 compat verify + A5-T12 runtime catalog ─▶ A2-T16 Proton (Linux) / A2-T17 Wine (macOS)
A3 desktop UI (mockIPC first) ───────────────▶ wires to real commands as A2/A4/A5 deliver
A3 admin web (MSW first) ────────────────────▶ wires to real API as A1 delivers
A5-T06 CLI, A5-T07 release pipeline, A5-T08 agent-written changelog, A5-T09 updater, A5-T11 adversarial tests ─▶ M4
```

## Waves

| Wave | Agent 1 | Agent 2 | Agent 3 | Agent 4 | Agent 5 |
|---|---|---|---|---|---|
| **1** (start together) | T01–T05 | T01–T03 | T01–T03, T13 (mocks) | T01–T02 | T01–T04 (**first**: interfaces within the first PRs) |
| **2** | T06–T13 | T04–T09 | T04–T08, T14–T16 | T03–T08 | T05–T08 |
| **3** | T14–T17 | T10–T18 | T09–T12, T17–T19 | T09–T13 | T09–T13 |

## Integration milestones (the orchestrator runs these demos)

| Milestone | Demo script | Gate |
|---|---|---|
| **M1 · Hello server** | `docker compose up`, API with the dev fake-Discord provider, launcher adds server (fingerprint shown), signs in, sees an empty catalog; admin web login works | A1-T01..T05, A2-T01..T02, A2-T07, A3-T01..T03, A3-T13 |
| **M2 · Publish → install** | `vgames publish` a 20 GB synthetic package (100k small files + 10 × 1 GB files) to fs storage → verify job → publish → launcher installs at line rate; kill the launcher at 40% → restart → resumes; disk usage never exceeds final size; flip one byte in a pack → install refuses before writing that chunk; launch a dummy exe; delete | A1-T07..T12, A2-T03..T09, A5-T02..T06 |
| **M3 · Social** | Two launcher profiles (`VGAMES_PROFILE=alice` / `bob`, debug builds): friend code, E2EE chat (server DB shows only ciphertext), Alice invites Bob to a package Bob lacks → install dialog opens → Alice sees progress → ready → join; overlay toast over a borderless test game | A4 all, A3-T09 |
| **M3b · Everywhere** | One Windows test package (D3D11) and one D3D12 package: install and play on Windows, on Linux through Proton, and on an Apple Silicon Mac through Wine (D3D11 via D3DMetal and via DXMT, D3D12 via D3DMetal; D3D12 refused up front on an Intel Mac); cloud save round-trips across all three; the in-game overlay shows an invite over an **exclusive-fullscreen** D3D11 game on Windows and over the Proton game on Linux; the NSPanel shows over a fullscreen Mac game | A2-T16..T17, A4-T10..T11, A5-T12 |
| **M4 · Release candidate** | CI release build (test signing keys), update from previous build showing only the user-facing changelog lines written by agents and curated by the release-notes agent; all budgets in 00-overview §7 measured and recorded; security test matrix green; 24 h soak clean | everything |

## Orchestrator checklist per merged PR

- Stayed in ownership area, or a `contract:` PR was approved first.
- Changelog fragment present; `audience: user` text is player-facing.
- Tests would fail without the change (spot-check one).
- No secret material, signed URLs, or real ids in the diff.
