//! Ordered, bounded launch extensions (PLAY-01). Futures and synchronous entry
//! points are both panic-contained; dropping a prepared launch unwinds hooks.

use std::collections::BTreeMap;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use vgames_core::manifest::{Manifest, Platform};

use super::LaunchPlan;
use super::orchestrate::LaunchError;
use crate::events::{GameExit, PackageRef};

pub type HookFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchLayer {
    Native,
    Proton,
    Wine,
}

#[derive(Debug, Clone)]
pub struct LaunchContext {
    /// Unique per launch or re-attachment; teardown must never key only by package.
    pub session_id: uuid::Uuid,
    /// Copied from the verified signed manifest. None on an unverified re-attachment.
    pub controllers: Option<vgames_core::manifest::Controllers>,
    pub package: PackageRef,
    pub root: PathBuf,
    /// Platform of the signed release, not the host.
    pub platform: Platform,
    pub layer: LaunchLayer,
    /// Derived only from AppPaths::data_dir and the package's UUIDs.
    pub prefix_dir: Option<PathBuf>,
    pub cancellation: CancellationToken,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HookEnv(pub Vec<(String, String)>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchVeto {
    SaveConflict { conflict_id: String },
}

impl From<LaunchVeto> for LaunchError {
    fn from(value: LaunchVeto) -> Self {
        match value {
            LaunchVeto::SaveConflict { conflict_id } => Self::SaveConflict { conflict_id },
        }
    }
}

/// Each stage may register one service. Registration order never affects execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HookStage {
    Saves,
    Controllers,
    Overlay,
}

pub trait LaunchHooks: Send + Sync + 'static {
    fn before_spawn<'a>(
        &'a self,
        ctx: &'a LaunchContext,
    ) -> HookFuture<'a, Result<HookEnv, LaunchVeto>>;

    #[cfg(windows)]
    fn pre_resume<'a>(
        &'a self,
        _ctx: &'a LaunchContext,
        _process: &'a super::process::SuspendedProcess,
    ) -> HookFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    fn after_exit<'a>(
        &'a self,
        _ctx: &'a LaunchContext,
        _exit: &'a GameExit,
    ) -> HookFuture<'a, ()> {
        Box::pin(async {})
    }

    /// Must also release partially prepared resources after panic or timeout.
    fn aborted(&self, ctx: &LaunchContext);

    fn before_spawn_budget(&self) -> Duration {
        Duration::from_secs(5)
    }

    fn after_exit_budget(&self) -> Duration {
        Duration::from_secs(30)
    }
}

/// Runtime download and prefix setup are cancellable, with no hook timeout.
pub trait PlanSource: Send + Sync + 'static {
    fn plan<'a>(
        &'a self,
        ctx: &'a LaunchContext,
        manifest: &'a Manifest,
    ) -> HookFuture<'a, Result<LaunchPlan, LaunchError>>;
}

#[derive(Default)]
pub struct HookRegistry {
    entries: Mutex<BTreeMap<HookStage, Arc<dyn LaunchHooks>>>,
    source: OnceLock<Arc<dyn PlanSource>>,
}

impl LaunchLayer {
    pub fn for_platform(build: Platform, host: Option<Platform>) -> Result<Self, LaunchError> {
        if host.is_some_and(|host| super::orchestrate::runs_natively(build, host)) {
            return Ok(Self::Native);
        }
        match (build, host) {
            (Platform::WindowsX86_64, Some(Platform::LinuxX86_64)) => Ok(Self::Proton),
            (Platform::WindowsX86_64, Some(Platform::MacosX86_64 | Platform::MacosAarch64)) => {
                Ok(Self::Wine)
            }
            _ => Err(compat_unavailable(
                "This release cannot run on this platform.",
            )),
        }
    }
}

impl HookRegistry {
    pub fn has_plan_source(&self) -> bool {
        self.source.get().is_some()
    }

    /// Startup registration only: never replace a service with live sessions.
    pub fn register(&self, stage: HookStage, hook: Arc<dyn LaunchHooks>) -> bool {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let std::collections::btree_map::Entry::Vacant(entry) = entries.entry(stage) {
            entry.insert(hook);
            true
        } else {
            false
        }
    }

    pub fn register_plan_source(&self, source: Arc<dyn PlanSource>) -> bool {
        self.source.set(source).is_ok()
    }

    pub async fn plan(
        &self,
        ctx: &LaunchContext,
        manifest: &Manifest,
    ) -> Result<LaunchPlan, LaunchError> {
        if ctx.cancellation.is_cancelled() {
            return Err(compat_unavailable("Launch cancelled."));
        }
        if ctx.layer == LaunchLayer::Native {
            return Ok(LaunchPlan::Native);
        }
        let source = self.source.get().ok_or_else(|| {
            compat_unavailable("This build needs a compatibility layer that is not available.")
        })?;
        let result = tokio::select! {
            () = ctx.cancellation.cancelled() => return Err(compat_unavailable("Launch cancelled.")),
            result = AssertUnwindSafe(async { source.plan(ctx, manifest).await }).catch_unwind() => result,
        };
        let plan = match result {
            Ok(Ok(plan)) => plan,
            Ok(Err(error @ LaunchError::CompatUnavailable { .. })) => return Err(error),
            Ok(Err(_)) | Err(_) => {
                return Err(compat_unavailable(
                    "Cannot prepare the compatibility layer.",
                ));
            }
        };
        // A compat failure must never turn into a direct Windows executable spawn.
        let valid = match (&plan, ctx.layer) {
            (LaunchPlan::Proton(plan), LaunchLayer::Proton) => {
                ctx.prefix_dir.as_ref() == Some(&plan.prefix)
            }
            (LaunchPlan::Wine(plan), LaunchLayer::Wine) => {
                ctx.prefix_dir.as_ref() == Some(&plan.prefix)
            }
            _ => false,
        };
        if !valid {
            return Err(compat_unavailable(
                "The compatibility plan does not match this launch.",
            ));
        }
        Ok(plan)
    }

    pub fn session(&self, ctx: LaunchContext) -> SessionHooks {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        SessionHooks {
            ctx,
            hooks: entries.values().cloned().collect(),
            prepared: Vec::new(),
            armed: true,
        }
    }
}

fn compat_unavailable(detail: &str) -> LaunchError {
    LaunchError::CompatUnavailable {
        detail: detail.into(),
    }
}

/// Owned by the preparation guard, then by the session tracker. On re-attach all
/// registered services receive after_exit; before_spawn is never repeated.
pub struct SessionHooks {
    pub(super) ctx: LaunchContext,
    hooks: Vec<Arc<dyn LaunchHooks>>,
    prepared: Vec<Arc<dyn LaunchHooks>>,
    armed: bool,
}

impl SessionHooks {
    pub async fn before_spawn(&mut self) -> Result<HookEnv, LaunchError> {
        let mut env = HookEnv::default();
        let hooks = self.hooks.clone();
        for hook in &hooks {
            if self.ctx.cancellation.is_cancelled() {
                self.abort_prepared();
                return Err(compat_unavailable("Launch cancelled."));
            }
            // The guard also owns a hook while its future is pending: a caller
            // dropping launch() must clean up partially prepared resources.
            self.prepared.push(hook.clone());
            // Include construction of the future and budget in panic containment.
            let ctx = &self.ctx;
            let work = AssertUnwindSafe(async {
                tokio::time::timeout(hook.before_spawn_budget(), hook.before_spawn(ctx)).await
            })
            .catch_unwind();
            let result = tokio::select! {
                () = self.ctx.cancellation.cancelled() => None,
                result = work => Some(result),
            };
            let Some(result) = result else {
                self.prepared.pop();
                abort(hook, &self.ctx);
                self.abort_prepared();
                return Err(compat_unavailable("Launch cancelled."));
            };
            match result {
                Ok(Ok(Ok(values))) => {
                    env.0.extend(values.0);
                }
                Ok(Ok(Err(veto))) => {
                    self.prepared.pop();
                    abort(hook, &self.ctx);
                    self.abort_prepared();
                    return Err(veto.into());
                }
                Ok(Err(_)) | Err(_) => {
                    tracing::warn!("launch hook timed out or panicked; continuing without it");
                    self.prepared.pop();
                    abort(hook, &self.ctx);
                }
            }
        }
        Ok(env)
    }

    fn abort_prepared(&mut self) {
        for hook in self.prepared.drain(..).rev() {
            abort(&hook, &self.ctx);
        }
    }

    pub(super) fn started(&mut self) {
        self.armed = false;
    }

    pub(super) fn reattached(&mut self) {
        self.prepared.clone_from(&self.hooks);
        self.started();
    }

    /// Bound the whole suspended phase; regardless of its result the caller resumes.
    #[cfg(windows)]
    pub async fn pre_resume(&self, process: &super::process::SuspendedProcess) {
        let work = async {
            for hook in &self.prepared {
                let result = AssertUnwindSafe(async { hook.pre_resume(&self.ctx, process).await })
                    .catch_unwind()
                    .await;
                if !matches!(result, Ok(Ok(()))) {
                    tracing::warn!("pre-resume hook failed or panicked; the game will resume");
                }
            }
        };
        if tokio::time::timeout(Duration::from_secs(2), work)
            .await
            .is_err()
        {
            tracing::warn!("pre-resume hooks timed out; the game will resume");
        }
    }

    pub(super) async fn after_exit(&self, exit: &GameExit) {
        for hook in &self.prepared {
            let work = AssertUnwindSafe(async {
                tokio::time::timeout(hook.after_exit_budget(), hook.after_exit(&self.ctx, exit))
                    .await
            })
            .catch_unwind();
            let result = tokio::select! {
                () = self.ctx.cancellation.cancelled() => {
                    abort(hook, &self.ctx);
                    continue;
                }
                result = work => result,
            };
            if !matches!(result, Ok(Ok(()))) {
                tracing::warn!("exit hook timed out or panicked");
                abort(hook, &self.ctx);
            }
        }
    }
}

impl Drop for SessionHooks {
    fn drop(&mut self) {
        if self.armed {
            self.abort_prepared();
        }
    }
}

fn abort(hook: &Arc<dyn LaunchHooks>, ctx: &LaunchContext) {
    if catch_unwind(AssertUnwindSafe(|| hook.aborted(ctx))).is_err() {
        tracing::warn!("launch hook cleanup panicked");
    }
}

#[cfg(test)]
mod tests;
