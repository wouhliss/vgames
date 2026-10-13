# PLAY status

## Done
- PLAY-04 profile persistence merged in [#203](https://github.com/wouhliss/vgames/pull/203) after all seventeen hosted checks passed. Native SQLite regressions prove restart persistence, server/package isolation, stable saved overrides, validation without partial writes and corrupt-row preservation.
- PLAY-03: macOS process-group/kqueue backend merged in [#195](https://github.com/wouhliss/vgames/pull/195) after all seventeen hosted checks passed, including native process/orchestrator tests, the blocking-kqueue regression and three-OS release builds.
- PLAY-02: Windows suspended spawn, bounded pre-resume, UTF-16 inputs, completion-port job tracking and stop merged in [#194](https://github.com/wouhliss/vgames/pull/194). All seventeen hosted checks passed on current main, including native Windows regressions and three-OS release builds. Final exit waits for the leader signal and releases completed stop handles; the post-exit re-attach and cancellable final-wait regressions pass.
- PLAY-01: ordered launch hooks, cancellable compatibility plans and session-owned exit callbacks merged in [#193](https://github.com/wouhliss/vgames/pull/193). Every hosted check passed on Windows, Linux and macOS, including the launcher browser performance test.

## In progress
- PLAY-01 hook fixture corrected after native Linux CI: a controlled FIFO holds the child until the test releases it, so startup duration cannot invalidate the early-exit assertion. Full process-tree and exactly-once re-attach checks remain unchanged.
- PLAY-04 native controller startup/list/tester integration implemented. Initialization and worker joining use the blocking pool; unavailable input is reported to the screen, Guide remains active in the background, and navigation follows main-window focus.
- PLAY-04 SDL routing adapter implemented: physical handles stay on their owner, cached metadata and scoped testers are exposed through Service, and releasing inputs cancels scheduled wakeups. Native startup/command integration follows separately.
- PLAY-04 typed controller event bridge implemented: navigation, active family, pad-list changes and scoped tester states are registered for generated bindings; Guide holds reuse the overlay bus event.
- PLAY-05 Windows Xbox output implemented through ViGEm with assigned XInput slots, request/stop serialization and CancelIoEx before feedback joining. Fake-driver report and failed-preparation cleanup regressions cross-compile with Clippy; native hosted proof, other target kinds and launch-session integration remain required.
- PLAY-05 macOS virtual output now checks the running process restricted entitlement through Security.framework, releases retained objects and fails closed. Unsigned builds cannot create CoreHID devices; native hosted proof remains required.
- PLAY-05 Linux virtual output implemented behind VirtualPadBackend: Xbox reports use a fixed event array, feedback waits on uinput/cancellation and only active effects arm deadlines. Rumble mixing, erase/stop, bounded uploads and cancellation regressions pass; hosted kernel latency/hot-plug/soak and launch-session integration remain required.
- PLAY-04/05 driver availability command implemented with native probes on the blocking pool and the scoped uaccess resource; virtual report forwarding and runtime entitlement verification remain separate PLAY-05 work.
- PLAY-08 external-link command implemented early for controller driver help: native validation restricts links to http(s), refuses credentials and IDN homographs, and never echoes rejected URLs. SafeMarkdown retains its existing confirmation flow; the pending function is replaced by generated bindings.


## Interfaces delivered (other agents may now rely on these)
- Inherited from Agent 2: verified launch targets and environment preparation (`launch::{target,plan,prelaunch}`), Linux pidfd/process-group tracking, session records and playtime, the typed event bus, SQLite worker/migration/settings APIs, strict deep-link parsing and shortcut files. Phase-1 status stays unchanged.
- Adopted INS's `compat::prefix_dir` and install/collection event additions. The INT-01 accessibility/import note is preserved below.

## Needs from others
- PLAY-04 controller notes proposed in [#196](https://github.com/wouhliss/vgames/pull/196) for INT review; implementation proceeds against the proposal.

## PLAY-04 pending IPC and mocks
- `play/p2-controller-ipc` defines pad metadata, profile/settings commands, tester selection and pad/tester events in `contract/controllers.ts`, with typed mock driver states and restart persistence. Four mock regressions, all 296 desktop UI tests, desktop typecheck and workspace lint pass. Local repository policy/security gates pass; all seventeen hosted checks passed in #198. Rust service and Settings screen follow separately, then these pending entries are removed in favor of generated bindings.

## PLAY-04 prerequisites
- #200 fresh native macOS checks exposed an intermittent `Cannot start the game` failure in the existing hook integration test. Added an error-level subscriber to preserve the native cause chain on the next run; the assertion stays unchanged. The same test passes locally on Linux. Diagnosis remains open; no check is skipped or rerun unchanged.
- Native routing now caches pad state, debounces navigation, selects active glyphs on meaningful input, emits one Guide hold per press and isolates/throttles tester subscriptions to at most thirty updates per second. Disconnect/stop remove subscriptions and their deadlines; SDL/service wiring follows.
- [#198](https://github.com/wouhliss/vgames/pull/198) merged the pending controller commands/events and persistent mock fixtures after all seventeen hosted checks passed. The launch/exit integration and deterministic cooldown rejection/expiry regressions pass on all three runners.
- SDL input lifecycle now owns mappings/gamepad handles on `vgames-sdl`, drains startup events before readiness and has latched control/shutdown wakeups. Linux checks actual no-pad voluntary switches over 60 s; Windows/macOS check zero loop iterations for 10 s. The exact-source Linux probe records 0 switches and 0.00 s CPU over 60 s. Remaining work includes native hosted evidence, event routing, commands/profiles, tester and Settings UI.
- Native no-pad wait adapters: Linux inotify/eventfd, Windows device-notification messages/event and macOS IOKit/CFRunLoop sources. Linux device-node lifecycle tests and cross-target Windows/macOS Clippy pass. The integrated SDL thread records zero voluntary switches and zero CPU over 60 s locally; native hosted checks remain required before merge.
- Per-game editor now uses generated profile commands for mode, Nintendo override, deadzones/anti-deadzones, Y inversion and sixteen button remaps. Unit tests prove restart persistence, isolation and gamepad range bounds. Full browser axe, keyboard restart and gamepad editing pass; fixed a shared select-menu scroll race found by those checks. Native session application follows in PLAY-05.
- Controllers screen: pad metadata/empty/unavailable states, persistent Nintendo labels, all seven driver states with confirmed help links and a scoped live tester. Thirteen unit regressions pass, including early events and late-start cleanup; full browser axe checks cover every driver state and the tester, plus keyboard restart and gamepad-only flows. Package overrides/remaps follow separately; native list/driver/tester commands remain in progress.
- Four settings/profile commands are now registered with generated types and only their main-window permissions. IPC sends complete mapping objects while legacy stored/imported mappings retain defaults; profile validation failures map to `invalid_input`. Command/capability synchronization and current-bindings tests pass. Their pending functions are removed; list, driver and tester commands remain pending until the input service lands.
- Controller profile storage validates before SQLite writes, isolates server/package rows, inherits global Nintendo labels for missing profiles and keeps saved per-package snapshots stable. Restart, invalid-update and corrupt-row regressions pass; typed commands and the screen follow next.
- #198 fresh CI exposed an existing integration test assuming game startup/exit completes within the three-second cooldown. Launch/exit remains tested; cooldown rejection, exact expiry and non-extension by rejected attempts now have a deterministic monotonic-clock regression.
- `play/p2-controller-input` adds the navigation state machine and SDL classification. All 19 controller tests (including six new navigation/classification regressions), formatting and full desktop Clippy pass locally. The SDL thread, commands, tester and Settings screen are still in progress; this branch is stacked on the static SDL build.
- Controller contract proposed in [#196](https://github.com/wouhliss/vgames/pull/196) for INT review; implementation can proceed against it.
- [#197](https://github.com/wouhliss/vgames/pull/197) merged static SDL3 after all hosted checks and three-OS builds passed. The native version-call test and full desktop Clippy pass locally; Linux development prerequisites are documented and appended to all desktop build jobs. Native three-OS linkage and all seventeen hosted checks passed before merge.

## Blockers / contract questions
- Repository write access and email privacy are resolved; PLAY commits use the account's GitHub no-reply address.

## Built for you
- From GAME (flaky test report, no code change): `controllers::idle::platform::tests::device_directory_creation_permissions_and_replacement_wake_without_polling`
  failed with "wait did not block" (`input_linux.rs:183`) on three unrelated GAME heads (#181, #187, #221). The
  `notification` helper asserts the next blocking wait does not return within 50 ms of the previous change; a late
  inotify event from that change (e.g. `IN_CLOSE_WRITE` after the `IN_CREATE` that already woke and drained the wait)
  can wake it early. Draining until the queue stays empty for a short quiet period before asserting the block would
  make it deterministic.
- From GAME (GAME-11): `bundle.resources` in `tauri.conf.json` now maps `src-tauri/resources/overlay/` to `overlay/`
  (the PLAY-09 slot). The renderers are staged there by CI and git-ignored; a committed `README.md` keeps the directory
  present for `tauri-build`. The launcher resolves them with `app.path().resource_dir()` and runs without them.
- From GAME (test fix, no product change): the Linux idle-wait test helper in `controllers/input_linux.rs` now drains
  a late event of the previous step (removing a watched directory queues `IN_IGNORED` only at inode eviction) and
  starts the wait again, failing if it never blocks within 5 tries. It reddened #181, #187, #221 and #227; under CPU
  load the old helper failed 1/60 runs, the new one 0/60.
- INT-01 prepares #103's reduced-motion setting for launcher mock accessibility scans, leaving the perf project unchanged. Updated Vitest imports use explicit `.ts` extensions with the matching no-emit compiler option to satisfy the newer loader. Adopt this note when the dependency triage PR merges.
- From INS (INS-04): appended to `events.rs` (append-only, README §5): the bus variants and UI events
  `InstallsChanged` (`installs-changed`) and `CollectionsChanged` (`collections-changed`). INS-03 appends
  `DownloadsChanged` (`downloads-changed`) the same way.
- From GAME (GAME-03): `build.rs` gains `embed_runtime_catalog_key()`, which copies `runtimes/runtime-catalog.pub`
  (INT-08) into `OUT_DIR` (an empty file without it) for `compat::catalog`; it watches `runtimes/`. Nothing else
  in the build changed.

## PLAY-01 integration API (branch `play/p2-launch-hooks`)

- `launcher.hooks().register(HookStage::{Saves, Controllers, Overlay}, Arc<dyn LaunchHooks>)` returns false for a duplicate stage. Hooks run in stage order regardless of registration order; registration finishes before startup re-attach.
- `LaunchHooks::before_spawn(&LaunchContext) -> HookFuture<Result<HookEnv, LaunchVeto>>` has a per-service budget (default 5 s); panic/timeout releases partial resources and skips only that hook. `LaunchVeto::SaveConflict` aborts prior hooks and reaches the existing `save_conflict` IPC error.
- `LaunchContext` supplies package, install root, signed release platform, `LaunchLayer::{Native, Proton, Wine}`, optional prefix derived through INS's `compat::prefix_dir`, and a shutdown cancellation token.
- `PlanSource::plan(ctx, verified_manifest)` runs before hooks, without a runtime-setup timeout, and can be cancelled. Register through `launcher.hooks().register_plan_source(source)`. Native launches remain native; absent, panicking, cancelled or mismatched compat plans fail closed. A source's `CompatUnavailable` detail passes through unchanged.
- Successful hooks belong to the game session. `after_exit(ctx, GameExit)` runs only after a confirmed whole-tree exit, including startup re-attach; detach and tracker errors retain records and never push saves. Dropped preparation unwinds partial hooks, including panicking cleanup. Exit work has a per-service budget (default 30 s) and shutdown cancellation.
- The Windows-only `SuspendedProcess` view and 2 s `SessionHooks::pre_resume` budget are defined here; PLAY-02 supplies its native handles and invokes it between suspended spawn and resume.
- Overlay is migrated to `before_spawn`, `after_exit` and `aborted`; its existing bus cleanup remains idempotent for callers outside the orchestrator and existing valve tests.

Registration example (Rust services, startup only):

```rust
use vgames_desktop_lib::launch::hooks::{HookStage, LaunchHooks, PlanSource};

fn register_launch_services<T: vgames_desktop_lib::launch::orchestrate::TrustSource>(
    launcher: &vgames_desktop_lib::launch::orchestrate::Launcher<T>,
    plans: std::sync::Arc<dyn PlanSource>,
    saves: std::sync::Arc<dyn LaunchHooks>,
    controllers: std::sync::Arc<dyn LaunchHooks>,
) {
    assert!(launcher.hooks().register_plan_source(plans));
    assert!(launcher.hooks().register(HookStage::Saves, saves));
    assert!(launcher.hooks().register(HookStage::Controllers, controllers));
}
```

## PLAY-02 Windows lifecycle and restart limits (branch `play/p2-windows`)

- `RunningGame::spawn_suspended` creates a non-inherited primary thread and process in a private unnamed Job Object, with an explicit application path, MSVC-quoted argv and a double-NUL UTF-16 environment. `GameSessions` records the session, awaits the total two-second pre-resume budget and resumes even when a hook errors, panics or times out. Dropping an unresumed spawn kills that process; dropping a resumed game never kills it.
- `SuspendedProcess::{process_handle, primary_thread_handle}` return borrowed native handles; `identity()` includes the PID and creation FILETIME. The view owns references so its handles stay valid for the asynchronous hook. No launcher handles are inherited by games.
- The running tree blocks in `GetQueuedCompletionStatus`; cancellation posts a separate packet and leaves the game alive. Whole-tree exit is confirmed through job accounting. Stop terminates the job after the existing UI confirmation; Windows has no generic graceful SIGTERM equivalent, so both stop modes terminate it.
- Restart does not reopen a job by name. It validates a live leader's creation time, assigns a fresh nested job, adopts discoverable live descendants with time/parent checks, and tracks later descendants through job inheritance. Access/assignment failures return an error and preserve the session record.
- Restart cannot recover an orphan tree whose leader already exited, nor descendants whose ancestry disappeared before the snapshot. It returns no attachment for a missing/exited/reused leader, without an exit callback or save push. Existing in-memory job tracking still waits for orphan children. These limits are explicit Windows test cases; Linux keeps its process-group recovery test.

## PLAY-03 macOS lifecycle (branch `play/p2-macos`)

- Games start in their own process group. The tracker blocks in `kevent` with no timeout; `EVFILT_PROC` watches exit/fork notifications, libproc discovers children and group members, and `EVFILT_USER` wakes cancellation. Cancelling or dropping launcher handles leaves games alive.
- The stored identity includes PID and BSD creation time. Re-attach validates the leader when present and can recover an orphan group; stop sends SIGTERM or SIGKILL to the verified group. Descendants that deliberately leave that group are outside tracking, matching Linux's scope.
- Missing/reused identities return no attachment. Permission, enumeration and registration errors fail closed and retain session records. Exit is confirmed only after every live group member disappears; native tests cover forked/orphan children, restart, stop, cancellation, escaped children and a one-second interval without periodic queue wakeups.

- PLAY-05 DS4 output uses pinned public ViGEm bus ABI byte buffers, retained per-report event/storage, neutral hat/axis and explicit XInput-to-DS4 conversion; blocking notification cancellation drains before join. Preserves the requested logical player order for DS4. Real-driver hardware proof remains checklist B; hosted ABI/conversion and no-driver checks are required.

- PLAY-05 ViGEm preparation has a single two-second deadline; timeout cancels and drains pending readiness I/O. DS4 control/report requests also cancel/drain before releasing stable request storage; feedback remains an indefinite driver wait with explicit stop cancellation. These deadlines exist only around submitted operations.
- PLAY-05 session resource core: decision/profile snapshot, held-state initial report, assigned player order, driver-failure passthrough, cancellation, and unique-UUID reservation/install/take. Teardown resources leave the registry before driver joins. Production launch-hook/SDL owner wiring and hiding remain follow-up work.
- PLAY-05 launch contexts now carry a fresh session UUID and controllers from the verified signed manifest. Re-attachment creates a fresh identity with no unverified controller configuration; restoring virtual outputs after re-attachment remains follow-up work behind contract #196.

- PLAY-05 lifecycle adapter: verified launch config and stored profiles prepare outputs on blocking workers; cached SDL reports route to sessions and bounded coalesced rumble returns to the SDL owner. Launch UUID teardown cancels builders and drops drivers outside locks. Three lifecycle regressions, all 51 controller tests and Clippy pass on the combined branch. Virtual-output filtering, physical hiding and mid-session hot-plug remain follow-ups; this is not full PLAY-05 completion.
- PLAY-05 virtual input filtering: Linux uses the original SDL joystick name; Windows checks native device ancestors for ViGEmBus, with assigned XInput slot fallback and a post-install refresh for devices announced during driver creation. All 51 Linux controller tests and Clippy pass; actual Windows ancestry code passes cross-Clippy. Hosted Windows evidence remains required.
- PLAY-05 session hot-plug now dispatches off-thread preparation from SDL add/wake events, using the launch's verified policy and stored profile. Concurrent builders reserve distinct indexes; duplicate wakes do not retry failed devices indefinitely. Disconnect during initial/hot-plug preparation rejects late outputs and feedback, and driver resources drop outside locks. Eight session tests and four launch-adapter tests plus Clippy pass; the earlier full 53-controller suite also passed. Physical hiding, visible fallback notices and trusted re-attachment restoration remain unfinished.
