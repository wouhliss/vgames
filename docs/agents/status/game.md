# GAME status (phase 2 · In-game & social)

## Done

## In progress

## Interfaces delivered (other agents may now rely on these)

## Needs from others

## Blockers / contract questions

## Built for you
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
