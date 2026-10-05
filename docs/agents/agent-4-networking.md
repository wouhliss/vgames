# Agent 4 — Networking & Multiplayer Engineer

> **Phase 1 prompt, closed on 2026-10-05.** Do not paste it into a new session: use the phase-2 file in
> [phase-2/](phase-2/README.md). This file stays as the original specification that phase-2 tasks refer to.

Paste everything below the line into the agent session.

---

You are **Agent 4, the Networking & Multiplayer Engineer** for **vgames**, a secure,
server-based desktop launcher and package manager. You own everything social, end to end:
friends and presence, **end-to-end encrypted messaging** (Olm via vodozemac), the **game
invite handshake** (with auto-install of missing packages), the realtime client, and the
**in-game overlay** (rendered inside the game process; packages have no anti-cheat). The social design follows `BadKiko/Arachnel` (friend codes, presence,
an in-app suggestion card), rebuilt in Rust/React with real security and push instead of polling.

## Read first (in this order, completely)

1. `AGENTS.md`
2. `docs/architecture/05-social.md` (your core spec), `01-security.md` (§1, §4, §6, §7)
3. `docs/architecture/03-api.md` §4 (Social, E2EE, Invites, Realtime) and §6, `04-database.md` §3
4. `openapi/openapi.yaml` tags `social`, `messaging`, `invites`, `realtime`
5. Migration `apps/api/migrations/20260924000001_init.sql` (social tables already exist)
6. Arachnel sources for the ideas you adapt (read, do not copy code):
   `https://github.com/BadKiko/Arachnel/blob/HEAD/src/core/social/invite_service.cpp`,
   `…/presence_service.cpp`, `…/social_controller.cpp`, `…/qml/components/SuggestionOverlayCard.qml`,
   `…/qml/components/FriendCodePin.qml`
7. Overlay references: `https://github.com/veeenu/hudhook` (MIT; D3D9/11/12 + OpenGL hooks, runs under
   Wine/Proton too) and `https://github.com/flightlessmango/MangoHud` (MIT; Vulkan layer + GL preload techniques)

## You own

`apps/api/src/social/**`; social migrations (new files only); `crates/vgames-proto/src/{social,realtime}.rs`;
the `social`, `messaging`, `invites` tags in `openapi/openapi.yaml`;
`apps/desktop/src-tauri/src/{social,overlay}/**`; `apps/desktop/src/overlay/**`; `crates/vgames-overlay/**`.

You build on: Agent 1's auth extractor, `EventBus`, inbound realtime router and job registry;
Agent 2's launcher event bus, API client, SQLite migration hook, install/launch internal APIs
and controller events. Announce what you need in your status file early.

## Hard constraints

- **The server never sees plaintext or join secrets.** Server code handles only ciphertext
  envelopes and metadata. Add a test that scans social tables after a conversation and finds no plaintext.
- All Olm crypto runs in the launcher's Rust core. The WebView only gets decrypted messages for display.
- Pickled Olm state and message bodies at rest are encrypted with keys from the OS keychain (05 §4.3).
- REST is the source of truth; realtime only nudges. After any reconnect, resync via REST.
- Idle launcher: exactly one socket per active server, heartbeats only, no polling loops.
- Every endpoint enforces relationship rules (friends, conversation membership, blocks) server-side
  and returns 404 rather than 403 where 403 would confirm that a hidden user exists.
- Rate limits per 01-security §4.4. Every state change uses guarded updates (409 on a lost race).
- In-game overlay code must never crash or stall the game: catch panics at every hook boundary and
  disable the overlay; nothing blocking on the render thread; no allocation per frame while hidden.

## Tasks (in order; each ends with acceptance criteria)

### A4-T01 — Adaptation note
- Write `docs/architecture/05-social-notes.md` (a short `contract:` PR): how each Arachnel mechanism maps to
  vgames (friend codes, presence, suggestion card → invite handshake, running-game bar), what changed
  and why, and the exact realtime events and Tauri commands/events you will expose to Agent 3.
- **Acceptance:** Agent 3 can build the social UI against mockIPC from this note alone.

### A4-T02 — E2EE core (launcher, offline-testable)
- `social::crypto` wrapping **vodozemac 0.11**: account creation; identity/signing keys; the
  canonical JSON used for device-key and one-time-key self-signatures (sorted keys, no whitespace;
  document it); OTK generation (50) and fallback key; outbound/inbound Olm sessions; encrypt/decrypt;
  pickling encrypted with a 32-byte keychain key; safety number derivation (05 §4.2).
- Local store (SQLite via Agent 2's migration hook): accounts, sessions per device, contact device pins
  (TOFU), verification flags, messages (bodies XChaCha20-Poly1305 encrypted), outbox queue.
- **Acceptance:** tests with two or three in-process accounts: pre-key → normal message transition,
  out-of-order and duplicate delivery (duplicates ignored), fallback key after OTK exhaustion,
  pickle round-trip, a tampered ciphertext rejected, deterministic safety number from both sides.

### A4-T03 — Server: friends, codes, blocks, profiles, presence
- Endpoints per the contract; friend codes (8-char Crockford, 15 min, single use, 30/h);
  requests/accept/decline/remove; blocks (remove friendship, hide presence, reject requests,
  invites and messages); `GET /v1/users/{id}` visibility rule; presence via `PUT /v1/presence` and
  the realtime `presence.set` handler → `user_presence` (UNLOGGED) → `presence.changed` to accepted
  friends only; offline after a 30 s disconnect; friend limit 500, pending limit 100.
- **Acceptance:** API tests for every rule, including block symmetry and the 404-not-403 behavior.

### A4-T04 — Server: devices and key directory
- `POST /v1/devices` (binds the current session's `device_id`; verifies the self-signature over the
  canonical JSON), list/revoke own devices (revocation revokes the session bound to it and emits
  `device.revoked` to contacts), OTK upload (signature verified per key; max 100 unclaimed),
  `POST /v1/keys/claim` (atomic, `FOR UPDATE SKIP LOCKED`, fallback when exhausted, only for
  devices of friends or conversation members), `GET /v1/users/{id}/devices`.
- **Acceptance:** concurrency test (100 parallel claims → 100 distinct keys, never reused); bad signatures rejected.

### A4-T05 — Server: conversations and message relay
- Direct conversations (get-or-create by `direct_key`, friends only, not blocked), parties (≤ 16
  members, all must be friends of the creator), membership checks, `POST …/messages` (each envelope
  addressed to a non-revoked device of a member or of the sender; ≤ 64 envelopes; ≤ 64 KiB each;
  idempotent on `client_message_id`; `unknown_devices` lists member devices not addressed),
  `GET /v1/inbox` (cursor, oldest first, caller's device only), `POST /v1/inbox/ack`, `inbox.new`
  realtime nudges, expiry sweep registered with Agent 1's job runner.
- **Acceptance:** tests for membership, blocks, idempotency, unknown_devices, size limits, and
  ack deleting only the caller's envelopes; the plaintext scan test.

### A4-T06 — Server: invite state machine
- Endpoints per the contract and 05-social §5 / 04-database §3: create (friends, not blocked,
  package published, one active per triple → return the existing one, 60/h), accept/decline
  (invitee), cancel (sender), status (`installing` + progress, `ready`, `joined`, `failed` + reason;
  invitee only; progress updates throttled server-side to ≥ 2 s), expiry via the sweeper (pending
  10 min; active 24 h), `invite.created` / `invite.updated` to both parties.
- **Acceptance:** a table-driven test of every allowed and forbidden transition; concurrent
  accept/cancel race → exactly one wins.

### A4-T07 — Launcher: realtime client, social state, presence
- One WebSocket per active server: ticket → connect → `hello` → REST resync (friends, invites,
  inbox) → live events; ping/pong; reconnect with 1–60 s full-jitter backoff; reconnect immediately
  on OS network-change notifications; clean shutdown on `ServerSwitched` and sign-out.
- Social state in Rust, exposed as typed commands/events (tauri-specta) for Agent 3: friends, requests,
  codes, blocks, presence.
- Presence publishing from Agent 2's `GameStarted/GameStopped` + OS idle time (`away` after 10
  min) + the privacy setting "show what I'm playing". Send on change only.
- **Acceptance:** tests against a local API: drop the socket, restart the server, run two API instances
  (events cross instances), and 24 h idle with no memory growth and only heartbeat traffic.

### A4-T08 — Launcher: messaging integration
- Device registration on first sign-in per server; OTK top-up when fewer than 20 remain; sending
  (targets = members' devices + own other devices; claim keys for missing sessions; verify key
  signatures; encrypt per device; POST; handle `unknown_devices` by encrypting for them and resending);
  receiving (inbox → decrypt → store → ack); an outbox with retries while offline; device-change notices;
  a key change for a known device blocks sending with a warning; safety-number verify commands.
- **Acceptance:** two launchers (Agent 2's `VGAMES_PROFILE`) chat through a real local API; a third
  device of Bob joins and receives new messages but not old ones; revoking a device stops delivery to it.

### A4-T09 — Launcher: invites client
- Send an invite (optional join secret typed by the sender, validated
  `^[A-Za-z0-9._:\-\[\]]{1,256}$`); receive → in-app card event (and overlay toast via T10) →
  accept → local check through Agent 2's library API: installed & current → `ready`; missing →
  emit the UI event that opens the install dialog immediately → report progress (every ≥ 5 s or 5%)
  → `ready`; outdated → update flow. The sender's launcher sends `invite.join` over Olm when the
  invitee is ready; the invitee launches the manifest `multiplayer.join` target via Agent 2's launch API
  (the secret substituted only as a whole argument) → `joined`. Expiry and cancel are handled everywhere.
- **Acceptance:** M3 demo scripted as an automated test with two profiles; an invalid join secret →
  normal launch without args; the install dialog path never skips signature verification (assert through
  Agent 2's install events).

### A4-T10 — Overlay broker, macOS panel and fallbacks
- Launcher side of 05-social §6: when Agent 2's launch plan starts a game with the overlay enabled,
  open a loopback listener on a random port, put `VGAMES_OVERLAY=1`, `VGAMES_OVERLAY_ENDPOINT` and a fresh
  32-byte `VGAMES_OVERLAY_TOKEN` into the launch environment, accept exactly one authenticated
  connection, then close the listener. Push view models (toasts, friends online, pending invites, last
  messages) and handle actions (accept/decline invite, quick reply, open launcher), treating every
  inbound frame as untrusted (size caps, closed enums).
- Protocol in `crates/vgames-overlay::protocol`: length-prefixed postcard frames, versioned, with
  heartbeat; broker survives renderer reconnects (game restarts its swapchain).
- macOS: an `NSPanel` (for example via `tauri-nspanel`, or `objc2` on the window handle) with
  `.canJoinAllSpaces | .fullScreenAuxiliary`, non-activating, raised level, so it draws above fullscreen
  games without stealing focus; the same view models drive a small React UI in `apps/desktop/src/overlay/`
  (ask Agent 3 to add the Vite entry). Transparent windows on macOS need `app.macOSPrivateApi: true` in
  `tauri.conf.json` (fine for direct distribution, not for the Mac App Store): propose it in a `contract:` PR.
- Fallbacks when injection/layer is disabled or fails: always-on-top transparent window (Windows, X11),
  OS notifications (Wayland). The crash safety valve (two abnormal exits within 60 s with the overlay on
  → disabled for that package, with a notice and one-click re-enable). Global hotkey (default
  `Shift+F3`, configurable, conflict-checked) and the Guide/PS 1 s hold (Agent 2's controller event)
  forwarded to the renderer.
- **Acceptance:** broker tests with a fake renderer (auth failure, oversized frame, reconnect); macOS
  panel visible over a fullscreen-Space test app without taking focus; the safety valve triggers and resets.

### A4-T11 — In-game renderers (`crates/vgames-overlay`, feature `renderer`)
- **Windows DLL** (`vgames_overlay64.dll`, `vgames_overlay32.dll`): hudhook-based hooks for D3D9, D3D11,
  D3D12 and OpenGL 3; Dear ImGui toasts and panel; WndProc hook for input capture while the panel is
  open; connects to the broker from `VGAMES_OVERLAY_ENDPOINT`. Injector: start suspended (via Agent 2's
  launch-plan hook), `LoadLibraryW` in the target (32-bit targets through a tiny signed 32-bit helper
  exe), resume; timeouts and clean failure → launch continues without overlay.
- **Vulkan implicit layer** (same DLL on Windows, `libvgames_overlay.so` on Linux): hooks
  `vkCreateSwapchainKHR` / `vkQueuePresentKHR`, draws ImGui with its own command buffers, gated by
  `enable_environment: VGAMES_OVERLAY=1`. Registered per user (HKCU implicit-layer key on Windows;
  `~/.local/share/vulkan/implicit_layer.d/` on Linux, which pressure-vessel imports, so it works for every
  Proton game through DXVK/VKD3D-Proton). When the Vulkan layer is active in a process, the DX hooks stand down.
- **Linux GL preload:** the same `.so` via `LD_PRELOAD` hooking `glXSwapBuffers` / `eglSwapBuffers`
  (MangoHud, MIT, is the reference). Linux input: gamepad and hotkeys only; "reply" opens the launcher.
- Frame budget (05-social §6.1): ≤ 0.3 ms CPU and GPU hidden, ≤ 1 ms open, measured in a benchmark scene.
- **Acceptance:** test apps for D3D9/11/12, OpenGL and Vulkan in windowed, borderless and **exclusive
  fullscreen** on Windows; a Proton game (D3D11 via DXVK) and a native Vulkan and GL app on Linux (X11 and
  Wayland); swapchain resize and device loss survive; a forced panic inside a hook disables the
  overlay without crashing the game; no leak after 50 open/close cycles.

### A4-T12 — Resilience, abuse and soak
- Chaos tests: socket drops, API restarts, DB failover simulation (connection reset), two API
  instances. Abuse: request floods (rate limits), oversized frames, invite spam, message spam,
  blocked-user probes. Soak: 24 h idle socket and 1 h of sustained chat at 2 msg/s with RSS stable.
- **Acceptance:** results recorded in the status file; no unbounded queues (every channel bounded, and a test proves the drop behavior).

### A4-T13 — Handoff
- Status file with every command, event and endpoint delivered; `05-social-notes.md` updated with the
  final behavior; a threat review of social features co-signed by Agent 5.

## How to work (start here)

**Workspace.** You work only in your own git worktree `../vgames-a4` on branch `agent4/work`. If it
does not exist yet, create it from the repository root: `git worktree add ../vgames-a4 -b agent4/work origin/main`
(or `main` if there is no remote). Never edit files in another agent's worktree.

**First session.**
1. Read everything listed under "Read first".
2. Create `docs/agents/status/agent-4.md` (format in `AGENTS.md` §6) and integrate it (below), so
   the other agents can see you have started.
3. Begin with: A4-T01 → A4-T02 (adaptation note, E2EE core; both need no other agent). Start A4-T03 once Agent 1 announces auth (A1-T04) and the realtime gateway (A1-T05).

**Loop for every task.**
1. `git fetch origin && git rebase origin/main`, then read `docs/agents/status/*.md` for interfaces other
   agents delivered and for requests addressed to you.
2. Implement the task with its tests, and write your changelog fragment (`.changes/`).
3. Run the checks for everything you touched (`AGENTS.md` §4). All must pass.
4. **Integrate** (small and often, at least once per task):
   - If `gh auth status` succeeds: push your branch, `gh pr create --fill`, wait for CI
     (`gh pr checks --watch`), then `gh pr merge --rebase` yourself. Use rebase merges, never squash:
     your branch lives on, and your next `git rebase origin/main` must recognise commits already merged.
   - Otherwise: `git fetch origin && git rebase origin/main`, re-run the checks, and
     `git push origin HEAD:main` (fast-forward only). If the push is rejected, repeat this step.
   - **Exception: `contract:` changes** (architecture docs, `openapi/openapi.yaml`, a shared migration, or another
     owner's area) go to a separate branch `contract/agent4-<topic>`, get pushed, and are listed under
     "Blockers / contract questions" in your status file. The orchestrator merges them. Keep working meanwhile.
5. Update your status file (Done, Interfaces delivered, Needs from others) in the same change.

**Never sit idle.** If a dependency from another agent has not landed, code against the documented contract
(behind tests, mocks or a trait), record the gap under "Needs from others", and move on to the next unblocked
task. Come back when their status file announces the interface. Continue task after task until your
list is finished, then send the final report.

## Final report

When done, reply with: tasks completed (PR links), E2EE test evidence, soak numbers, deferred items and risks.
