# Agent 2 status

## Done
- A2-T01 Desktop shell bootstrap (commit on `main`, "feat(desktop): shell bootstrap")
- A2-T02 Local database (commit on `main`, "feat(desktop): local SQLite database")
- A2-T03 `vgames-pack` planner, pack streams, verifier, WASM (commit on `main`, "feat(pack): …").
  Interim `layout`/`paths` modules until `vgames_core` lands (see Needs from others).

## In progress
- A2-T04 Download engine (`vgames-transfer::download`)

## Interfaces delivered (other agents may now rely on these)
- **Event bus** (A2-T01, for Agents 3, 4, 5): `vgames_desktop_lib::events::{EventBus, AppEvent}`,
  reached through `State<AppState>` (`state.bus.subscribe()` / `publish()`). Variants: `GameStarted`,
  `GameStopped`, `InstallProgress`, `InstallFinished`, `ServerSwitched`, `Controller(ControllerEvent)`
  (includes `ControllerChange::GuideHeld` for the overlay), `DeepLinkReceived { url }` (raw, untrusted).
  Subscribers must handle `RecvError::Lagged`. `AppState.shutdown` is the root `CancellationToken`.
- **Bindings pipeline** (A2-T01, for Agent 3): `apps/desktop/src/bindings.ts` (tauri-specta, `Result` error
  mode, error type `CommandError { code: ErrorCode, message }`). Commands: `appReady()` (call once the
  first screen rendered: the main window starts hidden), `appInfo()`. Events: `gameStarted`,
  `gameStopped`, `installProgress`, `installFinished`, `serverSwitched`, `controllerEvent`.
  Regenerate: `VGAMES_UPDATE_BINDINGS=1 cargo test -p vgames-desktop bindings`.
- **Window permissions** (A2-T01, for Agent 4): commands are listed in
  `apps/desktop/src-tauri/src/commands/names.rs`; `build.rs` turns them into `allow-<cmd>` permissions.
  The overlay window may call only what `OVERLAY_WINDOW_COMMANDS` lists (`overlay_*` only, test-enforced).
  To add overlay commands, send me the names (or a small PR touching `names.rs` + `capabilities/overlay.json`).
- **Debug profiles** (A2-T01, for M3): `VGAMES_PROFILE=<a-z0-9->` (debug builds) gives identifier
  `app.vgames.launcher.profile-<name>`, so its own data dir, logs, WebView storage and single-instance lock.

- **SQLite + migration hook** (A2-T02, for Agent 4): `state.db.call(|conn| { … Ok(x) }).await`
  runs on the dedicated `vgames-db` thread (`vgames_desktop_lib::db::{Db, DbError}`; `rusqlite` is re-exported
  as `db::rusqlite`). Social migrations: append a `Migration { name, sql }` to `db::migrations::MIGRATIONS`
  (append-only; name/BLAKE3 recorded in `schema_migrations` and checked at startup). Typed settings:
  `db::settings::{Setting, get, set}`.

- **`PackStreamVerifier`** (A2-T03, for Agent 1's `version.verify` job): `vgames_pack::verify::{PackStreamVerifier,
  PackExpectation, expectations_from_manifest}`. Build expectations from the (verified) manifest bytes with
  `PackExpectation::from_manifest(bytes, pack_index)`, `push(bytes, |verdict| …)` as pack bytes stream in
  (each `ChunkVerdict { index, result }` reports decode + BLAKE3; zstd bounded to the declared size), then
  `finish()` checks length and pack hash and returns the first failing chunk.
- **Packer library** (A2-T03, for Agent 5's CLI): `vgames_pack::{scan::scan, Plan::new, Plan::raw_packing,
  source::analyze (compression auto), PackSource::open_pack(i, offset) -> impl Read, manifest::build}`.
  Pack bytes are generated from source files at any offset; hashes (chunk, file, pack) are collected in the
  same pass, also when packs stream in parallel.
- **WASM package** (A2-T03, for Agent 3's admin upload worker): `packages/pack-wasm`
  (`pnpm --filter @vgames/pack-wasm build`, needs `cargo install wasm-pack`; output git-ignored). API:
  `WasmPacker.plan(files, dirs)`, `packCount/packChunks/chunkExtents/addChunk/buildManifest`, `Blake3Hasher`.
  Usage example in `crates/vgames-pack/src/wasm.rs`; a Node smoke test in `packages/pack-wasm/test/smoke.mjs`.
  Key-file decrypt + sign re-exports are added once A5-T03 lands.

## Measurements
- A2-T01 idle, Linux (WSLg, debug build, Vite dev server, software GL), 60 s window
  (`apps/desktop/perf/idle.py`): CPU 0.03% total (core 0.02%, WebKit web 0.02%, network 0.00%);
  context switches ≈ 1.4/s in total, all from WebKit/GTK. Launcher tokio workers: 0 wakeups.
  PSS 241 MB in total (core 88, web 141, network 12). This is a debug build with llvmpipe, so the < 200 MB budget
  is re-measured on a release build in A2-T14.
- Found and fixed: `blocking` 1.7 (via zbus ← single-instance/notification/keyring) wakes idle threads
  every 500 ms forever. `Cargo.lock` pins `blocking` 1.6.2 (idle threads exit). **Do not
  `cargo update` it back to 1.7** until upstream restores thread exit.

## Needs from others
- From Agent 3: call `commands.appReady()` once the first screen has rendered (until then the window shows
  after a 15 s fallback).
- From Agent 5: `vgames_core::{layout, paths, manifest}` (A5-T02) for A2-T03;
  `sign`, `trust`, `verify_manifest` (A5-T03/T04) for A2-T04 and A2-T07.
- From Agent 1: fs storage backend protocol (A1-T07) for transfer engine tests.
- From Agent 5 (CI, A5-T01): please add the WASM package build to CI:
  `cargo install wasm-pack` then `pnpm --filter @vgames/pack-wasm build && pnpm --filter @vgames/pack-wasm smoke`.
- From Agent 5 (manifest format): `vgames-pack` writes compact JSON in the 02 §5 field order, with
  `directories` always present (possibly `[]`) and `launch`/`controllers`/`saves`/`multiplayer` omitted when
  absent; `launch.targets[].args` always present, `working_dir`/`env` omitted when empty. Please make
  `vgames_core::manifest` accept exactly this (snapshot: `crates/vgames-pack/tests/snapshots/`).

## Blockers / contract questions
- Pre-existing Biome failures on `main` outside my area: `biome.json` (deprecated `recommended`, format)
  and `infra/gcs/cors.json` (format). Owners: Agent 5 / architect.
