# GAME status (phase 2 · In-game & social)

## PLAY-04 build integration
- `play/p2-sdl-static` enables SDL3 `build-from-source-static` without changing its version. Desktop builds now compile SDL3 with CMake and a C compiler; no system SDL3 installation is required.

## Done
- GAME-01 — F4, F5 (launcher), Q4 and the settings contract cleanup:
  [#182](https://github.com/wouhliss/vgames/pull/182), merged at `212ee98` (replaces #177, which INT reviewed).
  Evidence: `protocol::tests::hello_debug_never_prints_the_token`; `social::realtime::tests::a_2_mib_frame_is_refused_from_its_header`
  and three `decode_envelope` no-panic property tests; `social::api::tests::a_redirect_is_never_followed_and_the_token_stays_home`,
  `a_100_mib_body_is_refused_at_the_cap`, `a_full_inbox_page_at_the_maximum_ciphertext_size_is_accepted`; no pending
  `package_overlay`/`HotkeyError`/`SocialSettingsError` in `contract/settings.ts`. F4/F5/Q4 closure requested from INT-11 in `int.md`.
- GAME-02 — 09-compatibility v1.1 without D3DMetal: [#178](https://github.com/wouhliss/vgames/pull/178), adopted by INT
  on `main` as `2d95246` (+ `384010f`). The blocker rename and `compat::prefix_dir` were done by INS (INS-02, INS-04).
- Live on `main` from phase 1 (Agent 4, `agent-4.md` is the frozen record): A4-T01…T09 (social server, E2EE
  launcher core, realtime, messaging, invites with in-process fakes; `tests/social_chat.rs`); overlay broker,
  protocol, hub, safety valve, hotkey, fallback window and per-package switches (A4-T10 part 1); renderer link and
  guard, Linux Vulkan layer (`crates/vgames-overlay/tests/vulkan_layer.rs`, 22 µs/frame on lavapipe); chaos tests
  and 25-minute soak runs (`tests/social_chaos.rs`, `tests/social_soak.rs`).
- GAME-13 — social soak on hosted runners ([#230](https://github.com/wouhliss/vgames/pull/230)): `soak.yml` job
  `Social soak (chat|idle)`, `ubuntu-24.04`, the real API in process with a Postgres 18 service. RSS is the whole
  test process (API, pools and both launchers). Run [38339025747](https://github.com/wouhliss/vgames/actions/runs/38339025747)
  at `8bdb609`, 40 samples per job:
  - [chat](https://github.com/wouhliss/vgames/actions/runs/38339025747/job/115105615312), 1 h at 2 msg/s: 7201 messages
    sent and delivered, 0 duplicates, 0 drops. RSS (KiB): first 63968, after warm-up 68032, median 68734, end 70008
    (limit warm-up + 16384), last 70536. Slope after warm-up +3.7 MiB/h; flat (+2.5 MiB/h) from 21 to 46 min, then
    +6.7 MiB/h over the last quarter. Within the limit, so a 5 h 30 min chat run followed:
  - [chat, 5 h 30 min](https://github.com/wouhliss/vgames/actions/runs/38370146446/job/115204826528) at `49898cd`:
    39597 messages sent and delivered, 0 duplicates, 0 drops. RSS (KiB): first 65716, after warm-up 74648, median
    74794, end 76208 (limit warm-up + 16384), last 76332. Growth slows every quarter: +4.3, +3.1, +0.9, +0.2 MiB/h;
    the 1 h run's late climb was the end of warm-up, not a leak.
  - [idle](https://github.com/wouhliss/vgames/actions/runs/38339025747/job/115105615519), 5 h 30 min: 0 connection
    drops; the message sent after the idle period arrived. RSS (KiB): first 62788, after warm-up 62796, median 62800,
    end 62804 (limit warm-up + 8192), last 62804. Slope after warm-up +3 KiB/h, 0 over the last quarter.
  - The 24 h idle run is in human-checklist §B7 with its steps.

- GAME-03 — runtime manager: signed catalog per key ([#180](https://github.com/wouhliss/vgames/pull/180), `372f34a`,
  `77d79b2`), safe install, references and GC ([#181](https://github.com/wouhliss/vgames/pull/181), `252ab0d`,
  `d44b18d`), with INT's review fixes (no older catalog published over a newer one; the 6 h rule under the refresh
  lock; link-graph resolution and directory-handle confinement for extraction).
- GAME-04 — signed compat profiles per target ([#184](https://github.com/wouhliss/vgames/pull/184), `33d35a8`); social
  REST on the API client's transport with its caps (`1a69706`).
- GAME-05 — `CompatPlanner` as PLAY-01's `PlanSource`, pre-flight, `save_base` ([#185](https://github.com/wouhliss/vgames/pull/185),
  `07e72ff`; `b696871` makes an already-cancelled launch never get a plan); a D3D11 test game through real Proton on
  hosted `ubuntu-24.04` with lavapipe ([#187](https://github.com/wouhliss/vgames/pull/187), `ffb99ae`…`a4bf5be`,
  workflow `compat-e2e.yml` job `Proton`).
- GAME-06 — PE scan, D3D check on INS's priority-files hook, Rosetta, the Wine plan without D3DMetal, D3D12 refused
  before the download, hosted `macos-15` Wine job ([#220](https://github.com/wouhliss/vgames/pull/220), `8a37c26`).
- GAME-07 — the eight compat commands, per-package overrides (`0009_compat_overrides`), settings UI on the generated
  bindings, D3D12-on-every-Mac text ([#221](https://github.com/wouhliss/vgames/pull/221), `f4d8f0b`); the planner as
  INS's catalog `CompatProvider` ([#222](https://github.com/wouhliss/vgames/pull/222), `265d368`).
- GAME-12 (in-process part) — a game invite on the real install path (`64b46e3`); an outdated install updates before
  the invite is ready ([#255](https://github.com/wouhliss/vgames/pull/255), `e242c9c`).
- GAME-08 (most of it) — GL preload hooks for native Linux launches (`gl.rs`, `overlay::gl_preload`), Vulkan layer
  resize and device loss, Guide hold and hotkey panel input with "reply" opening the launcher, frame cost asserted and in
  the `ci` job summary ([#256](https://github.com/wouhliss/vgames/pull/256), `5d51ba9`). Local figures (debug,
  llvmpipe/lavapipe, 640×480): hidden +0.000 ms (EGL), within ±0.05 ms noise (GLX), +0.01 ms (Vulkan) per swap/present
  (≤ 0.3 ms); panel open in the hooks 0.08–0.10 / 0.17–0.20 / 0.05–0.06 ms, whole frame +0.23–0.32 / +0.44–0.51 /
  +0.13–0.24 ms (≤ 1 ms); opening the panel 14–21 ms GL, 5–6 ms Vulkan (bound 100 ms). The GL safety valve gives the
  first build in each window 1 s of warm-up and counts further builds in full
  ([#271](https://github.com/wouhliss/vgames/pull/271), `46d319d`; a loaded hosted runner took over 370 ms).
- GAME-11 (part) — `bundle.resources` overlay entries, the launcher resolving the renderers from the resource
  directory, and the `overlay-package-disabled` notice with "Turn it back on" (`OverlayDisabledNotice.tsx`).

## In progress
- GAME-08 — left: the Vulkan layer shown active in GAME-05's hosted Proton run; the fallback window on Wayland
  (today OS notifications) with a `weston --backend=headless` test.
- GAME-12 — left: the M3 two-instance scenario in `apps/desktop/e2e-real/`.
- GAME-14 — the 03-api §6 contract is [#252](https://github.com/wouhliss/vgames/pull/252) (reviewed by INT, green,
  waiting for INT's merge); left: the threat review, the `apps/desktop/README.md` compatibility section,
  `05-social-notes.md` final.
- Not started: GAME-09 (Windows DLL and injector), GAME-10 (macOS panel; needs the `macOSPrivateApi` contract),
  the rest of GAME-11 (32-bit DLL, helper and manifests in the matrix; installer contents).

## Interfaces delivered (other agents may now rely on these)
- `vgames_proto::realtime::MAX_FRAME` (64 KiB, GAME-01): the server re-exports it from `apps/api/src/realtime/mod.rs`;
  the launcher accepts at most `social::realtime::MAX_INBOUND_FRAME` (4 × `MAX_FRAME`).

## Needs from others

## Blockers / contract questions
- contract ack: #284 (Proton logging switches not profile or override keys; GAME implements it in #285).
- contract ack: #289 (wider env denylist; GAME's compat inputs are profiles and overrides, which share the list).
- contract ack: #290 (join secrets never option-shaped; GAME owns invites and the `invite.join` payload).
- Acks asked of the other slices: INS for #284 and #289 (publish, install and verify validate manifests and
  profiles), PLAY for #289 and #290 (native launch env; `TargetChoice::for_invite` and `join_args`).

## Built for you
- From INT (2026-10-24): contract replacement #339 merged at `f036081ccd0705a00ba21c9a7724a15cb5d4bdc6` after all applicable hosted checks passed, preserving acknowledgments and original authors. Controller #196 and validation #284/#289/#290 are closed with that superseding receipt. Their source implementations retain separate validation and merge requirements.
- From INT (INT-11, 2026-10-24): prepared a current-contract implementation adoption of #285's `cf28c3e`, #294's `a4c16ca` and #295's `dbf8ef3`, retaining GAME authors and fragments. Contract edits stay in the separate acknowledged batch; the later database retry `4147ee2` is not mixed into this security change. These are local preparations only: fresh base checks and full validation remain required before publication or merge, after the production broker repair.
- From INT (INT-01 / INT-11, 2026-10-23): adopting PLAY’s #323 port-retention repair as a main-health exception. Extra-client tests require EOF, connection reset or connection abort; timeout/WouldBlock fails. More than five extra clients are rejected while the original renderer remains usable, proving those clients do not exhaust failed-handshake policy. All 48 queue saturation cycles and the capacity assertion remain. A live Welcome/View witness replaces the 200 ms sleep before draining, and final disconnect delivery is asserted; the production port fix removes the need for the fixture-only EOF adapter. The disabled state after five failures also reserves the port; a full-event-queue regression requires channel closure even when Stopped was dropped. The fixture-only #327 is held pending the production regression proof.
- From PLAY (PLAY-11, #322 contract + this PR): `overlay::broker` keeps its listener bound for the broker's whole
  life and closes other connections unread while a renderer is served (`serve` takes `&TcpListener`; `rebind`,
  `REBIND_TRIES`/`REBIND_PAUSE` and `Task::addr` are gone). A released port could be taken by another local
  process and receive the reconnecting renderer's `Hello` and token; it also made
  `events_nobody_reads_are_dropped_and_never_block_the_broker` fail on #321 (a parallel test got the port). The
  test helper `reconnect` retries a close before `Welcome`, as `vgames_overlay::link` does; new test
  `the_port_stays_ours_while_a_renderer_is_served`. No renderer or wire change.
- From INT (integrator loop, zip advisories RUSTSEC-2026-0367/0368/0369): `compat::extract::extract_zip` moved to zip 9, where
  `ZipFile::name()` returns a `Result`; an unreadable (non-UTF-8) entry name now refuses the archive as `ExtractError::Corrupt`.
  One-line change, no behavior change for valid archives; the compat extract tests pass unchanged.
- INT adopted [#178](https://github.com/wouhliss/vgames/pull/178) on current main `54ce554` in `int/p2-game-compat-contract`, retaining GAME's contribution and its fragment. The contract matches the owner decisions and existing prefix/environment helpers; the priority-file wording accounts for shared chunks. Fresh current-base checks precede the replacement merge. No force-push or phase-1 status edit.
- From INS (INS-09, [#150](https://github.com/wouhliss/vgames/pull/150)): release launchers panicked at startup in
  `social::session_bridge::spawn` ("no reactor running"): Tauri's setup hook runs outside Tokio, so `setup()` now
  enters Tauri's runtime for its whole body. Keep `tokio::spawn` reachable from setup only under that guard, or use
  `tauri::async_runtime::spawn`.
- From INS (INS-02): the pending `CompatBlocker` kind `needs_apple_silicon` is renamed `d3d12_unsupported_on_mac`
  in `contract/catalog.ts`, `mocks/catalog.ts`, `routes/package/messages.ts` and `routes/package/CompatPanel.tsx`
  (pure rename; GAME-02's rename). When it applies (every Mac, owner decision 3) and its text stay GAME-07's.
- From INS (INS-04): `compat::prefix_dir(data_dir: &Path, package: &events::PackageRef) -> PathBuf` in the new
  `src-tauri/src/compat/mod.rs`, exactly as README §1 gives it (`<AppPaths::data_dir>/prefixes/<server_id>/<package_id>`
  on every OS), with a per-OS test. INS uses it for `has_prefix` and prefix removal on uninstall; `save_base` is yours.

- From INT (INT-06 restart proof): initialize the process TLS provider before service startup, matching the updater's ring provider. The real signed update diagnostic exposed a Rustls panic on realtime reconnect in the restarted process; HTTP and SQL features enable two providers. This startup repair changes no trust policy.
