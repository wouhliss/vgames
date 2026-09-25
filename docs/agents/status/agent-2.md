# Agent 2 status

## Done
- A2-T01 Desktop shell bootstrap (commit on `main`, "feat(desktop): shell bootstrap")
- A2-T02 Local database (commit on `main`, "feat(desktop): local SQLite database")
- A2-T03 `vgames-pack` planner, pack streams, verifier, WASM (commit on `main`, "feat(pack): …").
  Now on `vgames_core::{layout, paths}` (interim copies deleted); pack-wasm re-exports core's key-file/signing API.
- A2-T04 Download engine (`vgames-transfer::{download, install}`): see Interfaces and Measurements.
  https://github.com/wouhliss/vgames/pull/25
- A2-T05 Upload engine and publish flow (`vgames-transfer::upload`): see Interfaces.

## In progress
- None. Next: A2-T06 update, verify, repair, move, uninstall.

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
- **Publish library** (A2-T05, for Agent 5's CLI `vgames publish` and my A2-T13 admin mode):
  - `vgames_transfer::upload::plan_folder(root, Compression, threads) -> PlannedFolder` (blocking; refuses symlinks,
    special files and unsafe paths with `UploadError::InvalidTree(rejected)` for the plan preview).
  - `upload::publish(PublishRequest { package_id, version_label, platform, folder, execution, resume_file, publish },
    &dyn ManifestSigner, Arc<impl PublishApi>, &UploadOptions, &UploadControl) -> PublishReport`: create version (or
    resume the one in `resume_file`) → packs into GCS resumable sessions (16 MiB pieces, 4–16 packs in parallel,
    AIMD) → manifest → **local signature** (`SecretKey` implements `ManifestSigner`; decrypt the key file yourself
    and drop the key after) → manifest PUT → finalize → poll verification → optional publish.
    `UploadControl::{cancel, progress}` (phase, bytes, rate, ETA, parallel packs, verify progress).
  - **Agent 5:** implement `PublishApi` (7 methods, one per admin endpoint of 02 §6; map problem+json to
    `RemoteError { retryable, code, message }`, retryable = network/5xx/429) in the CLI's API client.
    `UploadError::remote_code()` gives the API code (`publisher_key_untrusted`, `publisher_key_not_yours`, …).
    The resume file holds signed session URIs: it is written `0600`; keep it in the user's app/config dir.
  - Test support: `testkit::{UploadRig (GCS/fs-backend resumable protocol with faults), MockPublishApi (finalize
    verifies like the server: size + hash, `verify_manifest` server mode, packs present, `PackStreamVerifier`)}`.

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
- From Agent 1 (FYI, not blocking): A2-T05 is tested against a simulator of the `fs` backend protocol
  (`apps/api/src/storage/fs.rs`), because the API cannot run in my sandbox (no Postgres 18). The end-to-end
  publish against a real local API is part of A2-T13.
- From Agent 3: call `commands.appReady()` once the first screen has rendered (until then the window shows
  after a 15 s fallback).
- From Agent 5 (manifest format): `vgames-pack` writes compact JSON in the 02 §5 field order, with
  `directories` always present (possibly `[]`) and `launch`/`controllers`/`saves`/`multiplayer` omitted when
  absent; `launch.targets[].args` always present, `working_dir`/`env` omitted when empty. Please make
  `vgames_core::manifest` accept exactly this (snapshot: `crates/vgames-pack/tests/snapshots/`).

## Blockers / contract questions
- Pre-existing Biome failures on `main` outside my area: `biome.json` (deprecated `recommended`, format)
  and `infra/gcs/cors.json` (format). Owners: Agent 5 / architect.
