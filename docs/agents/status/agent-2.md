# Agent 2 status

## Done
- A2-T01 Desktop shell bootstrap (commit on `main`, "feat(desktop): shell bootstrap")
- A2-T02 Local database (commit on `main`, "feat(desktop): local SQLite database")
- A2-T03 `vgames-pack` planner, pack streams, verifier, WASM (commit on `main`, "feat(pack): …").
  Now on `vgames_core::{layout, paths}` (interim copies deleted); pack-wasm re-exports core's key-file/signing API.
- A2-T04 Download engine (`vgames-transfer::{download, install}`): see Interfaces and Measurements.
- A2-T04 follow-up: bounded repeated expired links, final range length checks, stale journal invalidation, safe atomic metadata writes and linked-directory uninstall guard ([PR #38](https://github.com/wouhliss/vgames/pull/38)).
- A2-T04 CI follow-up ([PR #38](https://github.com/wouhliss/vgames/pull/38)): crash-resume tests wait for pack bytes before killing child processes on fast macOS runners; Windows Tauri test binaries embed the Common Controls v6 manifest, and generated bindings compare equal across CRLF/LF checkouts.
- A2-T05 Upload engine (`vgames-transfer::upload`): direct pack streams to GCS-style resumable sessions, private resume records, adaptive 4–16 workers, progress events, and end-to-end publishing API ([PR #39](https://github.com/wouhliss/vgames/pull/39), merged). The protocol is tested with a wiremock simulator and Agent 1's real fs storage backend; a separate process is killed after a randomly selected 256 KiB-aligned confirmed offset and the next process resumes; changing a file during streaming aborts with a clear error.
- A2-T06 signed-manifest update planner ([PR #40](https://github.com/wouhliss/vgames/pull/40), merged): identifies unchanged and changed files, removable paths, locally reusable chunks, remote bytes, and safe versus explicit in-place space requirements.
- A2-T06 read-only verifier ([PR #41](https://github.com/wouhliss/vgames/pull/41), merged): hashes installed files on bounded blocking workers and reports damaged file indices.
- A2-T06 durable update commit ([PR #50](https://github.com/wouhliss/vgames/pull/50), merged): replays interrupted signed-manifest replacements before an install becomes playable.
- A2-T06 safe transfer execution ([PR #51](https://github.com/wouhliss/vgames/pull/51), merged): stages changed files, reuses verified old chunks, and downloads only missing chunks.
- A2-T06 uninstall preview ([PR #55](https://github.com/wouhliss/vgames/pull/55), merged): lists leftovers before removal and preserves them unless the player chooses to delete them.
- A2-T06 install move ([PR #56](https://github.com/wouhliss/vgames/pull/56), merged): renames on one filesystem or copies and verifies signed files across filesystems, preserving user files and refusing links.
- A2-T06 repair and in-place update ([PR #57](https://github.com/wouhliss/vgames/pull/57), [PR #58](https://github.com/wouhliss/vgames/pull/58), merged): rebuilds damaged files and supports explicit low-space updates with crash replay.
- A2-T07 Servers, trust and sign-in in the launcher ([PR #47](https://github.com/wouhliss/vgames/pull/47)): see Interfaces.
  Mock-server tests cover TOFU pin, fingerprint-mismatch block (persisted, survives restart, lifts only when the
  pinned key returns), trust rollback and forged bundles refused, root rotation via `next_root`, refresh-token
  rotation (5 concurrent callers → one refresh), 401 → one refresh then one retry, refused refresh → local sign-out.
- A2-T08 library roots and registration ([PR #59](https://github.com/wouhliss/vgames/pull/59), [PR #60](https://github.com/wouhliss/vgames/pull/60), merged): validates and marks writable folders, detects offline drives, and stores libraries and their default choice on the SQLite thread.
- A2-T08 library removal ([PR #61](https://github.com/wouhliss/vgames/pull/61), merged): refuses libraries with installs or downloads, keeps player files, removes only its own marker, and promotes another default.
- A2-T08 library Tauri commands ([PR #64](https://github.com/wouhliss/vgames/pull/64), merged): list with offline status and install counts, pick/add folders, select default, and remove.
- A2-T08 collection and favorite storage ([PR #66](https://github.com/wouhliss/vgames/pull/66), merged): validated names, stable partial reorder, idempotent membership and favorite toggles.
- A2-T08 install queue storage ([PR #67](https://github.com/wouhliss/vgames/pull/67), merged): persisted jobs, one atomic active claim, and startup requeue.
- A2-T08 waiting-job reorder ([PR #70](https://github.com/wouhliss/vgames/pull/70), merged): atomic, exact-set queue ordering while an active transfer continues.
- A2-T08 cover cache core ([PR #71](https://github.com/wouhliss/vgames/pull/71), merged): 10 MiB raster entries, 500 MB on-disk LRU, opaque server-scoped keys, and Windows-safe recency updates.
- A2-T08 cache-only `vgimg://` protocol ([PR #72](https://github.com/wouhliss/vgames/pull/72), merged): strict opaque URL parsing and native cache reads on a blocking pool.

## In progress
- A2-T08 queue state transitions: atomic pause, resume, retry, failure and waiting-job removal; active jobs cannot be removed before worker shutdown.
- A2-T08 remote image fetch can now use the T07 API client.
- A2-T08 catalog, transfer orchestration, queue history, and collection/queue commands remain.
- A2-T09 launch: target resolution, launch plans and Linux tracking are done (below). Still to do: Windows
  (suspended spawn + pre-resume hook, Job Object + completion port) and macOS (kqueue `NOTE_EXIT`) trackers,
  the cloud-save and controller hooks (T11/T12) and compat plans (T16/T17) in `launch::orchestrate`.

- A2-T12 controllers: mapping tables and the emulation decision are done (below). Still to do: the SDL3 input thread,
  ViGEmBus/uinput/CoreHID backends, physical-pad hiding, rumble, the tester events, the latency benchmark and the soak test.

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

- **Servers, trust, sign-in** (A2-T07, for my A2-T08+ and Agents 3/4):
  - `state.servers: Arc<servers::Servers>`. `servers.api(server_id) -> ApiClient` is the only HTTP client:
    `public(method, "v1/…", body)` / `authed(…)` / `authed_empty(…)` return `api::ApiError` (`Problem{status, code,
    message}`, `Unauthenticated`, `TrustBlocked`, `Timeout`, `Network`, `Tls`, `InvalidResponse`); `AppError: From<ApiError>`.
  - `servers.trust_state(id)` (stored, no network) and `servers.refresh_trust(id)` (call it when the API or a
    manifest reports an unknown signing key); `servers.connect(id)` = identity check + trust refresh.
  - Commands (in `bindings.ts`): `serversList`, `serverPreview(url, expectedFingerprint)`, `serverConfirm(previewId)`,
    `serverSwitch`, `serverRemove`, `authStart`, `authOpenBrowser`, `authSubmitCode`, `authCancel`, `authSignOut`,
    `authTokenStorage` (→ `{ fallback_file }` for the Settings warning). Events: `serversChanged`,
    `connectivityChanged`, `trustProblem`, `serverAddRequested` (sent after `appReady`), `authFinished`.
    `ServerProfile.blocked_fingerprint` (optional) is set while a server is blocked, so a block found before the
    UI listened is still visible. Landed items were removed from `src/ipc/contract/core.ts` as its header asks.
  - Deep links: `deeplink::parse` (strict grammar) routes `server/add` and `auth/callback`; A2-T10 adds the rest.
  - DB: migration `0003_servers_trust`; new desktop migrations go after it.
- **Launch plans** (A2-T09, for Agent 4's invites and overlay, and my own T16/T17):
  `vgames_desktop_lib::launch::{target::resolve, TargetChoice, JoinSecret, TargetError, LaunchPlan::{Native, Proton, Wine},
  PreparedLaunch}`. `TargetChoice::for_invite(secret)` validates the join secret (05-social §5) and falls back
  to a normal launch when it is absent or invalid. `resolve` (blocking: call it from `spawn_blocking`) confines the
  executable and working directory to the install. `LaunchPlan::prepare(target, std::env::vars_os())` builds the
  program, argv, cwd and the complete environment. `PreparedLaunch::inject_env(key, value)` is the hook for the
  overlay endpoint, Vulkan layer or GL preload. It may not replace runner variables (`WINEPREFIX`, `PROTONPATH`, …).
- **Game processes** (A2-T09, Linux only so far): `launch::process::{RunningGame::{spawn, reattach, identity, wait,
  canceller, killer}, ProcessIdentity {pid, start_time}, GameExit, TreeKiller::terminate(force)}`. `wait` blocks on a
  dedicated thread until the whole process group has exited, with no timers. Processes that call `setsid`/`setpgid`
  leave tracking.
- **Launch commands** (A2-T09, for Agent 3): `gameLaunch(pkg, targetId)` → `LaunchError` (the contract's shape,
  including `busy { state: BusyState }` and `save_conflict`), `gameStop(pkg)` → `AppError`. The argument is `pkg`,
  not `package`. I updated the mock and the two test expectations, and removed the pending contract entries.
  Internally `state.launcher.launch(package, TargetChoice)` applies the 1-per-3 s limit, install state, trust
  (`servers.trust_state`), pre-launch checks and a native plan. For Agent 4's invites, call it with
  `TargetChoice::for_invite(secret)`.
- **Game sessions** (A2-T09, for Agents 3 and 4): `state.games: launch::GameSessions` with `start(package, root,
  prepared) -> pid`, `stop(package, force)`, `is_running`, `reattach`, `detach_all` (on exit). Publishes `GameStarted`
  and `GameStopped { code, stopped_by_user, session_seconds }`, adds playtime to `installs`
  (`db::installs::{record_playtime, playtime, roots}`), and keeps `<install>/.vgames/session.json` while a game runs.
  At startup every install is re-attached, and a game that runs across a launcher restart has its whole session credited.
- **Desktop shortcuts** (A2-T10, for Agent 3): command `shortcutCreate(pkg)` → path. The argument is `pkg`, not
  `package`, because `package` is reserved in strict TypeScript. I changed the one mock line in
  `src/mocks/library.ts` and removed the pending entry from `src/ipc/contract/library.ts`. The shortcut is named after the
  install folder until the catalog title cache exists. Uninstall calls `commands::shortcuts::remove_for_package`,
  which never deletes a file the player changed. Icons (`.ico`/PNG from the cover) and the `vgames://launch` route are
  next (#47 is merged, so `deeplink::parse` can take the route).
- **Controller mapping** (A2-T12): `controllers::mapping::{Mapper::new(kind, &Profile), Mapper::map(&PadState) ->
  XReport, Profile::parse(json)}` (the profile is the JSON in `controller_profiles.profile`) and
  `controllers::decision::{decide(&LaunchControllers, pad_kind) -> PadDecision, EmulationMode}`.
- **Pre-launch checks** (A2-T09): `launch::prelaunch::{Prelaunch::{check, forget}, Checked {manifest, target},
  PrelaunchError::{NotInstalled, Reverify, Integrity, ExecutableModified, …}}`. Call `forget(root)` after an
  update, repair, move or uninstall. Test support: `vgames_transfer::testkit::{TestPackage::build_executable,
  trust_state_with(version, revoke_publisher)}`.

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
- A2-T09 pre-launch check (`launch::prelaunch::tests::prelaunch_budget`, release, 4 vCPU Xeon, system temp dir,
  warm page cache): 200 MiB executable, first check **62 ms** (budget 300 ms), cached **14 µs** (budget 20 ms).
- Found and fixed: `blocking` 1.7 (via zbus ← single-instance/notification/keyring) wakes idle threads
  every 500 ms forever. `Cargo.lock` pins `blocking` 1.6.2 (idle threads exit). **Do not
  `cargo update` it back to 1.7** until upstream restores thread exit.

## Needs from others
- From Agent 3: call `commands.appReady()` once the first screen has rendered (until then the window shows
  after a 15 s fallback).
- From Agent 3 (A2-T08): add a generic failure variant to the pending `CollectionError` UI contract so collection commands can report SQLite failures without mislabeling them as a missing collection.
- From Agent 1 (A2-T08): queue history needs a new migration; `download_jobs` has no finished state or history table. Please agree on a durable history shape before the launcher exposes history commands.
- From Agent 5 (manifest format): `vgames-pack` writes compact JSON in the 02 §5 field order, with
  `directories` always present (possibly `[]`) and `launch`/`controllers`/`saves`/`multiplayer` omitted when
  absent; `launch.targets[].args` always present, `working_dir`/`env` omitted when empty. Please make
  `vgames_core::manifest` accept exactly this (snapshot: `crates/vgames-pack/tests/snapshots/`).

- From Agent 3: `ServerError` and `AuthError` have no "local failure" kind, so a local database error shows as
  `unreachable` / `server{code:"internal"}`. If you want an `internal { detail }` kind, add the case to
  `onboarding/messages.ts` and tell me; I will add the variant. `auth_token_storage().fallback_file` → Settings warning.

## Blockers / contract questions
- A2-T04 follow-up: directory components can be swapped for symlinks between validation and later file access; a directory-handle based path traversal is needed to close this local race across platforms. The non-racy uninstall traversal and atomic-write symlink cases are fixed with regressions.
