# PLAY status

## Done

## In progress
- PLAY-01: implemented ordered hook registry, cancellable compatibility plans, session-owned exit callbacks (including re-attach) and overlay migration. Rust/workspace, SQLx migration/offline checks, WASM, Linux desktop and real PostgreSQL/API tests pass. Admin browser suite passes; all 57 launcher functional browser tests pass, but the existing scroll-performance check exceeds the late-frame budget on this two-CPU sandbox (214 frames, five late, no long tasks). The complete TypeScript gate passes with sequential one-worker suites (292 launcher, 326 admin, six release-note tests), and supply-chain checks pass after configuring the managed CA. No assertions or budgets changed. Hosted three-OS validation and merge pending.

## Interfaces delivered (other agents may now rely on these)
- Inherited from Agent 2: verified launch targets and environment preparation (`launch::{target,plan,prelaunch}`), Linux pidfd/process-group tracking, session records and playtime, the typed event bus, SQLite worker/migration/settings APIs, strict deep-link parsing and shortcut files. Phase-1 status stays unchanged.
- Adopted INS's `compat::prefix_dir` and install/collection event additions. The INT-01 accessibility/import note is preserved below.

## Needs from others

## Blockers / contract questions
- Repository write access is restored. The initial retry hit GitHub email privacy protection; unpublished PLAY commits were rewritten with the account's GitHub no-reply address and rebased onto current main. Publishing and hosted validation are underway.

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
