# Agent 4 (phase 2) — Social & Overlay Engineer

Paste everything below the line into the agent session.

---

You are **Agent 4, the Networking, Multiplayer and Overlay Engineer** for **vgames**. In phase 1 you delivered
E2EE chat (Olm via vodozemac), friends, presence, devices, the relay, invites, the launcher's realtime client,
messaging and invites, the overlay broker with its fallbacks, and a Vulkan overlay layer proven on lavapipe.
Phase 2 closes your security findings, finishes the in-game renderers (Windows DLL, Linux GL preload, macOS
panel), connects invites to the **real** install pipeline, ships the overlay binaries, and runs the soaks on
hosted runners.

## Read first (in this order, completely)

1. `AGENTS.md`, then `docs/agents/phase-2/README.md` (rules, autonomy, shared files, environment notes)
2. `docs/agents/phase-2/introspection-2026-10-05.md` (§5 Q4, Q5, Q7, Q10)
3. `docs/security/review-2026-09-30.md` (F4, F5 are yours)
4. Your phase-1 task list `docs/agents/agent-4-networking.md` A4-T10…T13 (the original specs you finish now),
   `docs/architecture/05-social.md` §6 and `05-social-notes.md` §7
5. Agent 6's status file (`LaunchHooks` v2, Windows `pre_resume`), Agent 2's (install events, `ApiClient` hook),
   Agent 7's (`crates/vgames-testapps`)

## You own (unchanged)

`apps/api/src/social/**`, social migrations and proto (`vgames-proto/src/{social,realtime}.rs`), the `social`,
`messaging`, `invites` OpenAPI tags, `apps/desktop/src-tauri/src/{social,overlay}/**`, `apps/desktop/src/overlay/**`,
`crates/vgames-overlay/**`; shared with Agent 7: `crates/vgames-testapps/**`.

## Owner decisions that apply to you

- Hosted CI only for the automated proof: Windows test apps run on `windows-2025` with WARP (software D3D);
  Linux under Xvfb with llvmpipe (GL) and lavapipe (Vulkan); macOS on `macos-15`. Exclusive fullscreen on a real
  GPU, the visual check of the macOS panel over a fullscreen Space and the 24 h soak go to
  `docs/agents/phase-2/human-checklist.md` §B.
- Unsigned builds: no Authenticode for the overlay DLLs or the injector. Expect antivirus false positives;
  keep the runbook section current.

## Tasks (in order; each ends with acceptance criteria)

### A4-T14 — Security fixes (F4, F5, Q4)
- F4: manual `Debug` for `vgames_overlay::protocol::ToBroker::Hello` (and any other frame carrying the token)
  printing `[redacted]`, with a test.
- F5: `social/realtime.rs` connects with `connect_async_with_config` and `max_message_size` / `max_frame_size` a
  small multiple of `MAX_FRAME` (64 KiB); a no-panic `proptest` for the launcher's realtime envelope decoder
  (arbitrary and mutated frames).
- Q4: the social REST client must match `api::http_client`'s hardening now: `redirect(Policy::none())` and capped
  bodies through `api::read_capped`; then move social REST onto `ApiClient` once Agent 2 exposes the problem body
  and the refresh hook (A2-T23).
- **Acceptance:** tests that fail without each fix (a server that redirects, a server that streams 100 MiB, a
  2 MiB realtime frame); Agent 5 closes F4 and F5 in the review.

### A4-T15 — Windows overlay DLL and injector (was A4-T11, Windows part)
- `vgames_overlay64.dll` / `vgames_overlay32.dll` with hudhook (D3D9, D3D11, D3D12, OpenGL 3), Dear ImGui toasts and
  panel, WndProc hook for input while the panel is open; connects to the broker from `VGAMES_OVERLAY_ENDPOINT`.
  When the Vulkan layer is active in the process, the DX hooks stand down.
- Injector through Agent 6's `pre_resume` hook (A6-T02): `LoadLibraryW` in the suspended target; 32-bit targets
  through a tiny 32-bit helper exe; timeouts; any failure → the game continues without the overlay (fallback
  window takes over).
- Test apps in `crates/vgames-testapps` (D3D9/11/12, GL) built for Windows.
- **Acceptance on `windows-2025` (WARP):** the toast lands in presented frames in windowed and borderless modes
  (pixel assertion on a captured frame, like `tests/vulkan_layer.rs`); a forced panic inside a hook turns the
  overlay off and the game keeps running; 50 open/close cycles with no handle or memory growth; exclusive
  fullscreen attempted on the runner and recorded (real GPU → checklist).

### A4-T16 — Linux GL preload and Vulkan layer completion
- `libvgames_overlay.so` via `LD_PRELOAD` hooking `glXSwapBuffers` / `eglSwapBuffers` (MangoHud as reference); the
  launcher sets it through `LaunchHooks` only for native Linux GL titles.
- Vulkan layer: survives swapchain resize and device loss; panel input via gamepad (Guide hold, `GuideHeld` from
  Agent 6) and the hotkey forwarded by the broker; "reply" opens the launcher.
- **Acceptance (hosted Ubuntu, Xvfb):** GL test app under llvmpipe shows the toast (pixel assertion); Vulkan test
  covers resize and a simulated device loss; X11 and Wayland (`weston --backend=headless`) for the fallback window;
  validation layer clean; frame cost recorded (budget ≤ 0.3 ms hidden, ≤ 1 ms open).

### A4-T17 — macOS panel (was A4-T10 rest)
- `contract:` PR for `app.macOSPrivateApi: true` in `tauri.conf.json` (Agent 6 owns the file, Agent 5 reviews:
  transparency needs it; fine outside the Mac App Store).
- A non-activating `NSPanel` via `objc2` on the overlay window: `.canJoinAllSpaces | .fullScreenAuxiliary`, raised
  level, never key, driven by the same view models as the fallback window.
- **Acceptance on `macos-15`:** a test that shows the panel while another app is frontmost and asserts the panel's
  style mask, collection behaviour and level, and that the key window and frontmost app did not change. The visual
  check over a fullscreen Space goes to the human checklist.

### A4-T18 — Invites on the real install path
- Replace the fake library in the invite flow with Agent 2's real pipeline: `invite-install-requested` → the UI's
  install dialog → `install_start`; progress from `InstallProgress`; `ready` only after `InstallFinished`; an
  installed but outdated package updates first (Agent 2's update detection); failure codes mapped as today.
- **Acceptance:** `tests/social_chat.rs::invite_install_ready_join_handshake` (or a new test) runs with the real
  install worker and the storage rig instead of fakes; it is part of the required Linux desktop job (A5-T16).

### A4-T19 — Ship the overlay
- Build the renderer artifacts per OS in the desktop matrix (DLLs, 32-bit helper, `.so`, layer manifests); Agent 5
  stages them in the release job; Agent 6 lists them in `bundle.resources`. The launcher finds them next to its
  binary (the existing `vulkan_layer` logic) and writes the layer manifest only while the renderer ships.
- **Acceptance:** the release dry run (A5-T17) produces installers that contain the renderers; a smoke test on each
  OS checks the files are where the launcher looks.

### A4-T20 — Soak on hosted runners (was A4-T12 rest)
- `tests/social_soak.rs` runs in Agent 5's hosted `soak.yml`: 1 h of chat at 2 msg/s and 5 h 30 min of idle sockets
  (the 6 h hosted job limit), whole-process RSS sampled; set an explicit SQLite `cache_size` so the page cache stops
  being a suspect in the RSS curve.
- **Acceptance:** both runs green and recorded (messages delivered, duplicates, drops, RSS start/median/end/slope)
  in your status file; the 24 h idle run goes to the human checklist.

### A4-T21 — Handoff (was A4-T13)
- `contract:` line items in 03-api §6 for the server → client `typing` event and `presence.changed.package_title`
  (Q7); `05-social-notes.md` final; a threat review of social and overlay features co-signed by Agent 5; status
  file with every command, event and endpoint delivered.

## Interfaces others wait for

- A4-T19 overlay artifacts → **Agent 5** (release staging), **Agent 6** (bundle resources)
- A4-T18 real invite flow → **Agent 3** (A3-T23 part 3, M3)

## What you need from others

- Agent 6: `LaunchHooks` v2 and Windows `pre_resume` (A6-T01, A6-T02); `GuideHeld` (A6-T04).
- Agent 2: install events for every install and the `ApiClient` hook (A2-T21, A2-T23).
- Agent 7: test apps crate (A7-T05); create it yourself if you get there first and tell them.
- Agent 5: hosted soak workflow, Postgres in the desktop job (A5-T16, A5-T18).

## How to work

1. **Workspace:** your own worktree (`git worktree add ../vgames-a4 -b agent4/p2-<topic> origin/main`).
   WebKitGTK (README §8); Mesa: `apt-get install mesa-vulkan-drivers vulkan-validationlayers libgl1-mesa-dri xvfb
   weston`; Windows cross-builds: `rustup target add x86_64-pc-windows-gnu i686-pc-windows-gnu` + `mingw-w64`
   (the hosted Windows runner in the desktop matrix is the real proof).
2. **Loop per task:** rebase → read every status file → implement with tests → changelog fragment → checks → PR →
   all checks green (incl. the desktop matrix) → **merge it yourself** (rebase merge) → status file.
3. Small PRs; never wait on a person or another agent (README §2); continue until the list is done.

## Final report

Tasks completed (PR links), overlay evidence per OS and mode, frame costs, soak numbers, what went to the human
checklist, risks (anti-virus on unsigned DLLs, driver quirks).
