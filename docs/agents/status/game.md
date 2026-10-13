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

## In progress
- GAME-03 — runtime manager: signed catalog per key ([#180](https://github.com/wouhliss/vgames/pull/180)), safe
  install, references and GC ([#181](https://github.com/wouhliss/vgames/pull/181)); both carry INT's review fixes
  (no older catalog published over a newer one; due check under the refresh lock; link-graph resolution and
  directory-handle confinement for extraction).
- GAME-04 — compat profiles ([#184](https://github.com/wouhliss/vgames/pull/184)); social REST onto `ApiClient` next.
- GAME-05 — `CompatPlanner` as PLAY-01's `PlanSource`, pre-flight, `save_base` ([#185](https://github.com/wouhliss/vgames/pull/185));
  test apps and the hosted Proton run ([#187](https://github.com/wouhliss/vgames/pull/187)).
- GAME-06 — PE scan, D3D check on INS's priority-files hook, Rosetta, Wine plan (`game/p2-wine-macos`, not pushed yet).
- GAME-08 — GL preload hooks in `libvgames_overlay.so` (`game/p2-gl-preload`). Frame cost on llvmpipe, release, 640×480,
  one toast visible: 0.28 ms (EGL) / 0.32 ms (GLX) per frame, upload included (05-social §6.1: ≤ 1 ms open); hidden: one
  atomic load per swap. Launcher preload for native Linux launches next.
- GAME-04 (social part) — social REST on the API client's transport, caps kept (`game/p2-social-apiclient`).

## Interfaces delivered (other agents may now rely on these)
- `vgames_proto::realtime::MAX_FRAME` (64 KiB, GAME-01): the server re-exports it from `apps/api/src/realtime/mod.rs`;
  the launcher accepts at most `social::realtime::MAX_INBOUND_FRAME` (4 × `MAX_FRAME`).

## Needs from others

## Blockers / contract questions
- INT reviews of #180/#181 are answered on each PR (fixes in `8b552ab`/`dc88e09` and their rebases); re-review requested.

## Built for you
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

- Built for you (PLAY-06, GAME-05 fallback): `compat::save_base(&prefix, RunnerKind::Proton, SaveBase::Documents)` returns the Windows-relative Documents base inside the package prefix. Wine uses the native macOS account name; install returns None for the caller's verified install root. Shared resolver tests cover all bases and refused account-name escapes, with native macOS lookup checked by the matrix. Use this resolver from compatibility/cloud-save services rather than a second path mapping.
