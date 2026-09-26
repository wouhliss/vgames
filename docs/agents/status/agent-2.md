# Agent 2 status

## Done
- A2-T01 Desktop shell bootstrap (commit on `main`, "feat(desktop): shell bootstrap")
- A2-T02 Local database (commit on `main`, "feat(desktop): local SQLite database")
- A2-T03 `vgames-pack` planner, pack streams, verifier, WASM (commit on `main`, "feat(pack): …").
  Now on `vgames_core::{layout, paths}` (interim copies deleted); pack-wasm re-exports core's key-file/signing API.
- A2-T04 Download engine (`vgames-transfer::{download, install}`): see Interfaces and Measurements.
- A2-T04 follow-up: bounded repeated expired links, final range length checks, stale journal invalidation, safe atomic metadata writes and linked-directory uninstall guard ([PR #38](https://github.com/wouhliss/vgames/pull/38)).
- A2-T04 CI follow-up ([PR #38](https://github.com/wouhliss/vgames/pull/38)): crash-resume tests wait for pack bytes before killing child processes on fast macOS runners; Windows Tauri test binaries embed the Common Controls v6 manifest, and generated bindings compare equal across CRLF/LF checkouts.
- A2-T05 Upload engine (`vgames-transfer::upload`): direct pack streams to GCS-style resumable sessions, private resume records, adaptive 4–16 workers, progress events, and end-to-end publishing API ([PR #39](https://github.com/wouhliss/vgames/pull/39), stacked on #38). The protocol is tested with a wiremock simulator and Agent 1's real fs storage backend; a separate process is killed after a randomly selected 256 KiB-aligned confirmed offset and the next process resumes; changing a file during streaming aborts with a clear error.

## In progress
- A2-T06 update, verify, repair, move, and uninstall: the signed-manifest planner ([PR #40](https://github.com/wouhliss/vgames/pull/40)) identifies unchanged files, changed files, removable paths, locally reusable chunks, remote bytes, and safe versus explicit in-place space requirements. A read-only verifier ([PR #41](https://github.com/wouhliss/vgames/pull/41)) hashes installed files on bounded blocking workers, reports damaged file indices, and feeds a repair plan that excludes damaged chunks from local reuse. Transfer execution, commit replay, move, and uninstall prompts remain to be implemented.

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

- **Download engine** (A2-T04, for my own A2-T08 and for Agent 4's invites through it):
  - `vgames_transfer::install::{fetch_release, verify_release, load_local_release, install, remove_install,
    read_record, check_space, free_dir_name}`: `fetch_release(client, &ManifestLink, envelope, &TrustState,
    &ExpectedRelease, installed_sequence, VerifyMode, stall)` downloads the manifest (size + BLAKE3) and runs
    `verify_manifest`; `install(root, Arc<Release>, Arc<impl PackUrlSource>, &DownloadOptions, &DownloadControl)`
    → `InstallOutcome::{Installed(InstallRecord), Paused(User|DiskFull), Cancelled}`. Call it again to resume
    (journal in `.vgames/journal.bin`). `InstallError::is_integrity()` = never retry automatically.
  - `vgames_transfer::download::{DownloadControl (pause/resume/cancel/set_limit/progress watch), Progress
    {phase, bytes_done, bytes_total, bytes_per_second, eta_seconds, connections} (≤ 4 Hz), PackUrlSource
    (the launcher API client implements `pack_urls` = `POST /v1/versions/{vid}/download-urls` and
    `report_integrity`), RemoteError, DownloadError, run + DownloadSpec (used by update/repair)}`.
  - `vgames_transfer::http::{transfer_client (HTTP/1.1 only, no content decoding), redact_url}`.
  - Test support for other crates' tests: feature `testkit` (`TestPackage` signed packages, `Rig` loopback
    storage with fault injection, `MockApi`).
- **Upload engine (A2-T05, for Agent 5's CLI):** `vgames_transfer::upload::{run, UploadApi, UploadOptions, UploadControl}` streams `PackSource` packs through resumable sessions. `vgames_transfer::upload::publish::{create_version, run, PublishApi, PublishRequest, PublishOptions, PublishControl}` creates a version with a caller-persisted idempotency key, then resumes an existing version through signing, manifest upload, verification and publication. The caller must persist the returned `Version` before `publish::run` and place the private resume record in app data, outside the source tree.

## Measurements
- A2-T01 idle, Linux (WSLg, debug build, Vite dev server, software GL), 60 s window
  (`apps/desktop/perf/idle.py`): CPU 0.03% total (core 0.02%, WebKit web 0.02%, network 0.00%);
  context switches ≈ 1.4/s in total, all from WebKit/GTK. Launcher tokio workers: 0 wakeups.
  PSS 241 MB in total (core 88, web 141, network 12). This is a debug build with llvmpipe, so the < 200 MB budget
  is re-measured on a release build in A2-T14.
- A2-T04 loopback benchmark (`tests/bench_loopback.rs`, release build, 4 vCPU Xeon @ 2.1 GHz, tmpfs, storage rig
  in the same process): 2 GiB package, download phase **2.4–3.0 GB/s** (7 runs; one outlier at 1.37 GB/s on the
  shared VM), whole install including preallocation and finalize 1.9–2.3 GB/s; 4 GiB: 2.42 GB/s. Target ≥ 1.5 GB/s.
- A2-T04 memory (`tests/resilience.rs`, child process, 32 fixed connections, 512 MiB package, 8 MiB ranges): peak
  RSS (VmHWM, includes code pages) **243 MiB** debug / 237 MiB release; 48 buffers of 4 MiB are 192 MiB of it.
  Budget 256 MiB.
- A2-T04 crash safety: 50 random `SIGKILL`s of a child mid-install (10 of them killed twice), resumed in the parent:
  every tree byte-identical. Disk usage sampled every ms during an install never exceeded the final footprint plus
  3 blocks (journal, its temp file, the install record's temp file). No chunk is verified twice unless retried
  (asserted in every fault-free test and across pause/resume).
- Found and fixed: `blocking` 1.7 (via zbus ← single-instance/notification/keyring) wakes idle threads
  every 500 ms forever. `Cargo.lock` pins `blocking` 1.6.2 (idle threads exit). **Do not
  `cargo update` it back to 1.7** until upstream restores thread exit.

## Needs from others
- From Agent 3: call `commands.appReady()` once the first screen has rendered (until then the window shows
  after a 15 s fallback).
- From Agent 5 (manifest format): `vgames-pack` writes compact JSON in the 02 §5 field order, with
  `directories` always present (possibly `[]`) and `launch`/`controllers`/`saves`/`multiplayer` omitted when
  absent; `launch.targets[].args` always present, `working_dir`/`env` omitted when empty. Please make
  `vgames_core::manifest` accept exactly this (snapshot: `crates/vgames-pack/tests/snapshots/`).

## Blockers / contract questions
- PRs #38–#41 cannot merge while the required GitHub Actions checks fail before runner assignment (empty runner names and no job steps on repeated attempts). The account's Actions-minute allowance appears exhausted; the repository owner must restore runner access. The stack is rebased on `main` at `07f0861` and passes local formatting, strict Clippy, Rust tests with the development database, changelog lint, and TypeScript lint/typecheck/tests. The Windows manifest fix still needs its matrix run.
- A2-T04 follow-up: directory components can be swapped for symlinks between validation and later file access; a directory-handle based path traversal is needed to close this local race across platforms. The non-racy uninstall traversal and atomic-write symlink cases are fixed with regressions.
