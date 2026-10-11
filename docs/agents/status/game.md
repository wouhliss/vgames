# GAME status (phase 2 · In-game & social)

## Done

## In progress

## Interfaces delivered (other agents may now rely on these)

## Needs from others

## Blockers / contract questions
- INT review of [#177](https://github.com/wouhliss/vgames/pull/177#issuecomment-6108469677): social success bodies cannot use the generic 4 MiB cap. An inbox page contains up to 100 envelopes, each permitting 64 KiB of ciphertext before base64 (API `social/relay.rs` checks decoded size); a valid page can exceed 8 MiB. Use a justified 16 MiB social cap and an actual HTTP maximum-page regression, retaining the 64 KiB error cap and 100 MiB stream refusals. Avoid logging attacker-controlled serde values. Main advanced after #176; publish a fresh current-base replacement, preserving the original head and checks. This does not wait on another slice.

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
