# PLAY status

## Done
- PLAY-02: Windows suspended spawn, bounded pre-resume, UTF-16 inputs, completion-port job tracking and stop merged in [#194](https://github.com/wouhliss/vgames/pull/194). All seventeen hosted checks passed on current main, including native Windows regressions and three-OS release builds. Final exit waits for the leader signal and releases completed stop handles; the post-exit re-attach and cancellable final-wait regressions pass.
- PLAY-01: ordered launch hooks, cancellable compatibility plans and session-owned exit callbacks merged in [#193](https://github.com/wouhliss/vgames/pull/193). Every hosted check passed on Windows, Linux and macOS, including the launcher browser performance test.

## In progress
- PLAY-03: implemented macOS process-group tracking with kqueue, PID/start-time validation, child discovery, whole-group stop and cancellation without killing. Portable process/orchestrator fixtures now run on macOS; added a native idle-wakeup measurement. Cross-target Clippy passes and all 54 Linux launch regressions pass (one existing benchmark ignored). The native macOS suite passed, including the idle-wakeup measurement; fresh full validation runs after inheriting the latest Windows fix. Rebased onto merged PLAY-02/current main; #195 is retargeted to main for fresh validation.

## Interfaces delivered (other agents may now rely on these)
- Inherited from Agent 2: verified launch targets and environment preparation (`launch::{target,plan,prelaunch}`), Linux pidfd/process-group tracking, session records and playtime, the typed event bus, SQLite worker/migration/settings APIs, strict deep-link parsing and shortcut files. Phase-1 status stays unchanged.
- Adopted INS's `compat::prefix_dir` and install/collection event additions. The INT-01 accessibility/import note is preserved below.

## Needs from others
- PLAY-04 controller notes proposed in [#196](https://github.com/wouhliss/vgames/pull/196) for INT review; implementation proceeds against the proposal.

## PLAY-04 pending IPC and mocks
- `play/p2-controller-ipc` defines pad metadata, profile/settings commands, tester selection and pad/tester events in `contract/controllers.ts`, with typed mock driver states and restart persistence. Four mock regressions pass and desktop typecheck passes; full UI validation pending. Rust service and Settings screen follow separately, then these pending entries are removed in favor of generated bindings.

## PLAY-04 prerequisites
- Controller contract proposed in [#196](https://github.com/wouhliss/vgames/pull/196) for INT review; implementation can proceed against it.
- [#197](https://github.com/wouhliss/vgames/pull/197) merged static SDL3 after all hosted checks and three-OS builds passed. The native version-call test and full desktop Clippy pass locally; Linux development prerequisites are documented and appended to all desktop build jobs. Native three-OS linkage and hosted checks are required before merge.

## Blockers / contract questions
- Repository write access and email privacy are resolved; PLAY commits use the account's GitHub no-reply address.

## Built for you
- INT-01 prepares #103's reduced-motion setting for launcher mock accessibility scans, leaving the perf project unchanged. Updated Vitest imports use explicit `.ts` extensions with the matching no-emit compiler option to satisfy the newer loader. Adopt this note when the dependency triage PR merges.
- From INS (INS-04): appended to `events.rs` (append-only, README §5): the bus variants and UI events
  `InstallsChanged` (`installs-changed`) and `CollectionsChanged` (`collections-changed`). INS-03 appends
  `DownloadsChanged` (`downloads-changed`) the same way.

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
