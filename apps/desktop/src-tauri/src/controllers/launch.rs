//! Launch hooks own virtual outputs; SDL only borrows the session book to route
//! cached input. Driver destruction and joins run outside locks on blocking workers.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::runtime::Handle;
use uuid::Uuid;

use super::backends::{Rumble, RumbleSink, VirtualPadBackend};
use super::handle::Controllers;
use super::input::InputWake;
use super::mapping::PadState;
use super::profiles::Profiles;
use super::sessions::{Book, PhysicalPad, Policy, prepare};
use crate::events::GameExit;
use crate::launch::orchestrate::{
    HookEnv, HookFuture, LaunchContext, LaunchHooks, LaunchLayer, LaunchVeto,
};

#[derive(Default)]
struct Feedback {
    values: BTreeMap<(Uuid, u32), Rumble>,
    dirty: BTreeSet<u32>,
}
#[derive(Default)]
pub struct Bridge {
    pub(super) book: Mutex<Book>,
    feedback: Mutex<Feedback>,
    wake: OnceLock<InputWake>,
    backend: OnceLock<Arc<dyn VirtualPadBackend>>,
}
impl Bridge {
    pub fn attach(&self, wake: InputWake) {
        let _ = self.wake.set(wake);
    }
    fn wake(&self) {
        if let Some(wake) = self.wake.get()
            && let Err(error) = wake.wake()
        {
            tracing::warn!(%error, "cannot wake controller sessions");
        }
    }
    pub fn report(&self, id: u32, state: &PadState) {
        if let Ok(mut book) = self.book.lock() {
            book.report(id, state);
        }
    }
    pub fn hotplug(self: &Arc<Self>, pad: &PhysicalPad, runtime: &Handle) {
        let Some(backend) = self.backend.get() else {
            return;
        };
        let requests = self
            .book
            .lock()
            .ok()
            .map(|mut book| book.hotplug(pad))
            .unwrap_or_default();
        for request in requests {
            let bridge = self.clone();
            let backend = backend.clone();
            runtime.spawn_blocking(move || {
                let prepared = prepare(
                    request.id,
                    request.policy,
                    &[request.pad],
                    backend.as_ref(),
                    &|pad| bridge.sink(request.id, pad),
                    &request.cancel,
                );
                if let Ok(prepared) = prepared {
                    let leftovers = match bridge.book.lock() {
                        Ok(mut book) => book.install_hotplug(prepared),
                        Err(_) => Box::new(prepared),
                    };
                    drop(leftovers);
                    bridge.wake();
                }
            });
        }
    }
    pub fn disconnected(&self, id: u32, runtime: &Handle) {
        let removed = self.book.lock().ok().map(|mut book| book.disconnect(id));
        runtime.spawn_blocking(move || drop(removed));
        if let Ok(mut feedback) = self.feedback.lock() {
            feedback.values.retain(|(_, pad), _| *pad != id);
            feedback.dirty.remove(&id);
        }
    }
    pub fn feedback(&self) -> Vec<(u32, Rumble)> {
        let Ok(mut feedback) = self.feedback.lock() else {
            return Vec::new();
        };
        let dirty = std::mem::take(&mut feedback.dirty);
        dirty
            .into_iter()
            .map(|pad| {
                let mut combined = Rumble::default();
                for ((_, id), value) in &feedback.values {
                    if *id == pad {
                        combined.large = combined.large.max(value.large);
                        combined.small = combined.small.max(value.small);
                        combined.duration_ms = combined.duration_ms.max(value.duration_ms);
                    }
                }
                (pad, combined)
            })
            .collect()
    }
    fn sink(self: &Arc<Self>, session: Uuid, pad: u32) -> RumbleSink {
        let bridge = self.clone();
        Arc::new(move |value| {
            // Serialize acceptance with session removal; a driver notification
            // arriving after cancellation cannot restart physical rumble.
            if let Ok(book) = bridge.book.lock()
                && book.accepts_feedback(session, pad)
                && let Ok(mut feedback) = bridge.feedback.lock()
            {
                let key = (session, pad);
                if feedback.values.contains_key(&key) || feedback.values.len() < 64 {
                    feedback.values.insert(key, value);
                    feedback.dirty.insert(pad);
                }
            }
            bridge.wake();
        })
    }
    fn finish(&self, session: Uuid, runtime: &Handle) {
        let removed = self
            .book
            .lock()
            .ok()
            .and_then(|mut book| book.take(session));
        if let Ok(mut feedback) = self.feedback.lock() {
            let pads: Vec<_> = feedback
                .values
                .keys()
                .filter_map(|(id, pad)| (*id == session).then_some(*pad))
                .collect();
            feedback.values.retain(|(id, _), _| *id != session);
            feedback.dirty.extend(pads);
        }
        self.wake();
        runtime.spawn_blocking(move || drop(removed));
    }
    pub fn shutdown(&self, runtime: &Handle) {
        let removed = self.book.lock().ok().map(|mut book| book.drain());
        runtime.spawn_blocking(move || drop(removed));
    }
}

pub struct Hooks {
    profiles: Profiles,
    bridge: Arc<Bridge>,
    snapshot: Arc<dyn Fn() -> Vec<PhysicalPad> + Send + Sync>,
    backend: Arc<dyn VirtualPadBackend>,
    runtime: Handle,
}
impl Hooks {
    pub fn new(profiles: Profiles, controllers: Controllers, runtime: Handle) -> Self {
        let bridge = controllers.sessions.clone();
        Self {
            profiles,
            bridge,
            snapshot: Arc::new(move || controllers.physical_pads()),
            backend: backend(),
            runtime,
        }
    }
}
fn backend() -> Arc<dyn VirtualPadBackend> {
    #[cfg(target_os = "linux")]
    {
        Arc::new(super::backends::linux::Linux)
    }
    #[cfg(windows)]
    {
        Arc::new(super::backends::windows::Windows)
    }
    #[cfg(target_os = "macos")]
    {
        Arc::new(super::backends::macos::Macos)
    }
}
impl LaunchHooks for Hooks {
    fn before_spawn<'a>(
        &'a self,
        ctx: &'a LaunchContext,
    ) -> HookFuture<'a, Result<HookEnv, LaunchVeto>> {
        Box::pin(async move {
            let _ = self.bridge.backend.set(self.backend.clone());
            let cancel = self
                .bridge
                .book
                .lock()
                .ok()
                .and_then(|mut book| book.begin(ctx.session_id));
            let Some(cancel) = cancel else {
                return Ok(HookEnv::default());
            };
            let profile = match self.profiles.get(ctx.package).await {
                Ok(profile) => profile,
                Err(_) => {
                    self.bridge.finish(ctx.session_id, &self.runtime);
                    tracing::warn!("controller profile unavailable; keeping physical input");
                    return Ok(HookEnv::default());
                }
            };
            let policy = Policy {
                manifest: ctx.controllers.clone(),
                compat_layer: ctx.layer != LaunchLayer::Native,
                mode: profile.mode,
                mapping: profile.mapping,
            };
            let pads = (self.snapshot)();
            let bridge = self.bridge.clone();
            let backend = self.backend.clone();
            let id = ctx.session_id;
            let result = self
                .runtime
                .spawn_blocking(move || {
                    let prepared = prepare(
                        id,
                        policy,
                        &pads,
                        backend.as_ref(),
                        &|pad| bridge.sink(id, pad),
                        &cancel,
                    );
                    match prepared {
                        Ok(prepared) => {
                            if !prepared.notices().is_empty() {
                                tracing::warn!(
                                    "some physical controllers require a virtual controller driver"
                                );
                            }
                            // Rejected resources are dropped on this worker after the
                            // lock guard leaves scope, including timed-out preparations.
                            let rejected = match bridge.book.lock() {
                                Ok(mut book) => book.install(prepared).err(),
                                Err(_) => Some(Box::new(prepared)),
                            };
                            drop(rejected);
                            bridge.wake();
                        }
                        Err(_) => bridge.finish(id, &Handle::current()),
                    }
                })
                .await;
            if result.is_err() {
                self.bridge.finish(id, &self.runtime);
            }
            Ok(HookEnv::default())
        })
    }
    fn after_exit<'a>(&'a self, ctx: &'a LaunchContext, _: &'a GameExit) -> HookFuture<'a, ()> {
        Box::pin(async move {
            self.bridge.finish(ctx.session_id, &self.runtime);
        })
    }
    fn aborted(&self, ctx: &LaunchContext) {
        self.bridge.finish(ctx.session_id, &self.runtime);
    }
}

#[cfg(test)]
mod tests {
    use super::super::backends::{BackendError, EmulatedController, VirtualPad};
    use super::super::decision::EmulationMode;
    use super::super::mapping::{Button, XReport};
    use super::super::profiles::ControllerProfile;
    use super::*;
    use crate::db::Db;
    use crate::events::{ControllerKind, PackageRef};
    use tokio_util::sync::CancellationToken;
    use vgames_core::manifest::{Controllers as ManifestControllers, Platform};

    #[derive(Default)]
    struct Trace {
        reports: Vec<XReport>,
        drops: usize,
        feedback: Vec<RumbleSink>,
    }
    struct Fake(Arc<Mutex<Trace>>);
    struct Pad(Arc<Mutex<Trace>>, u16);
    impl VirtualPadBackend for Fake {
        fn create(
            &self,
            _: EmulatedController,
            index: u16,
            feedback: RumbleSink,
        ) -> Result<Box<dyn VirtualPad>, BackendError> {
            self.0.lock().unwrap().feedback.push(feedback);
            Ok(Box::new(Pad(self.0.clone(), index)))
        }
    }
    impl VirtualPad for Pad {
        fn player_index(&self) -> u16 {
            self.1
        }
        fn update(&mut self, report: &XReport) -> Result<(), BackendError> {
            self.0.lock().unwrap().reports.push(*report);
            Ok(())
        }
    }
    impl Drop for Pad {
        fn drop(&mut self) {
            self.0.lock().unwrap().drops += 1;
        }
    }
    fn setup() -> (Hooks, LaunchContext, Arc<Mutex<Trace>>) {
        let trace = Arc::new(Mutex::new(Trace::default()));
        let mut state = PadState::default();
        state.set(Button::South, true);
        let hooks = Hooks {
            profiles: Profiles::new(Db::open_in_memory().unwrap()),
            bridge: Arc::default(),
            snapshot: Arc::new(move || {
                vec![PhysicalPad {
                    instance_id: 7,
                    kind: ControllerKind::Dualsense,
                    player_index: None,
                    state,
                }]
            }),
            backend: Arc::new(Fake(trace.clone())),
            runtime: Handle::current(),
        };
        let ctx = LaunchContext {
            session_id: Uuid::new_v4(),
            controllers: Some(ManifestControllers {
                supported: vec![vgames_core::manifest::ControllerKind::Xinput],
                emulate_as: Some(EmulatedController::Xbox360),
            }),
            package: PackageRef {
                server_id: Uuid::new_v4(),
                package_id: Uuid::new_v4(),
            },
            root: "/fixture".into(),
            platform: Platform::LinuxX86_64,
            layer: LaunchLayer::Native,
            prefix_dir: None,
            cancellation: CancellationToken::new(),
        };
        (hooks, ctx, trace)
    }
    async fn drops(trace: &Mutex<Trace>, count: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if trace.lock().unwrap().drops == count {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn persisted_policy_initial_input_reports_and_exit_cleanup_use_the_launch_identity() {
        let (hooks, ctx, trace) = setup();
        hooks.before_spawn(&ctx).await.unwrap();
        assert_eq!(
            trace.lock().unwrap().reports.first().unwrap().buttons,
            0x1000
        );
        hooks.bridge.report(7, &PadState::default());
        assert_eq!(trace.lock().unwrap().reports.last().unwrap().buttons, 0);
        let next = LaunchContext {
            session_id: Uuid::new_v4(),
            ..ctx.clone()
        };
        hooks.before_spawn(&next).await.unwrap();
        hooks
            .after_exit(
                &ctx,
                &GameExit {
                    code: Some(0),
                    stopped_by_user: false,
                    session_seconds: 1,
                },
            )
            .await;
        drops(&trace, 1).await;
        assert!(
            hooks
                .bridge
                .book
                .lock()
                .unwrap()
                .session_mut(next.session_id)
                .is_some()
        );
        hooks.aborted(&next);
        drops(&trace, 2).await;
        hooks
            .profiles
            .set(
                ctx.package,
                ControllerProfile {
                    mode: EmulationMode::Never,
                    ..ControllerProfile::default()
                },
            )
            .await
            .unwrap();
        hooks.before_spawn(&ctx).await.unwrap();
        assert_eq!(trace.lock().unwrap().feedback.len(), 2);
        hooks.aborted(&ctx);
    }
    #[tokio::test]
    async fn feedback_is_coalesced_mixed_and_stale_callbacks_cannot_restart_rumble() {
        let (hooks, ctx, trace) = setup();
        let next = LaunchContext {
            session_id: Uuid::new_v4(),
            ..ctx.clone()
        };
        hooks.before_spawn(&ctx).await.unwrap();
        hooks.before_spawn(&next).await.unwrap();
        let callbacks = trace.lock().unwrap().feedback.clone();
        let first = callbacks.first().unwrap();
        let second = callbacks.get(1).unwrap();
        for large in 1..=1000 {
            first(Rumble {
                large,
                small: 100,
                duration_ms: 300,
                delay_ms: 0,
            });
        }
        second(Rumble {
            large: 400,
            small: 200,
            duration_ms: 400,
            delay_ms: 0,
        });
        assert_eq!(
            hooks.bridge.feedback(),
            [(
                7,
                Rumble {
                    large: 1000,
                    small: 200,
                    duration_ms: 400,
                    delay_ms: 0
                }
            )]
        );
        assert!(hooks.bridge.feedback().is_empty());
        hooks.aborted(&ctx);
        first(Rumble {
            large: 60000,
            ..Rumble::default()
        });
        assert_eq!(hooks.bridge.feedback().first().unwrap().1.large, 400);
        hooks.aborted(&next);
        second(Rumble {
            small: 60000,
            ..Rumble::default()
        });
        assert_eq!(hooks.bridge.feedback(), [(7, Rumble::default())]);
        drops(&trace, 2).await;
    }
    #[tokio::test]
    async fn compatibility_launches_and_shutdown_preserve_passthrough_and_cancel_builders() {
        let (hooks, mut ctx, trace) = setup();
        ctx.layer = LaunchLayer::Proton;
        hooks.before_spawn(&ctx).await.unwrap();
        assert!(trace.lock().unwrap().reports.is_empty());
        hooks.aborted(&ctx);
        ctx.layer = LaunchLayer::Native;
        hooks.before_spawn(&ctx).await.unwrap();
        let pending = hooks
            .bridge
            .book
            .lock()
            .unwrap()
            .begin(Uuid::new_v4())
            .unwrap();
        hooks.bridge.shutdown(&hooks.runtime);
        drops(&trace, 1).await;
        assert!(pending.is_cancelled());
        assert!(!hooks.bridge.book.lock().unwrap().contains(ctx.session_id));
    }
    #[tokio::test]
    async fn owner_hotplug_dispatch_builds_once_routes_new_input_and_drops_off_thread() {
        let (hooks, ctx, trace) = setup();
        hooks.before_spawn(&ctx).await.unwrap();
        let mut state = PadState::default();
        state.set(Button::East, true);
        let pad = PhysicalPad {
            instance_id: 9,
            kind: ControllerKind::Dualsense,
            player_index: None,
            state,
        };
        hooks.bridge.hotplug(&pad, &hooks.runtime);
        hooks.bridge.hotplug(&pad, &hooks.runtime);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let ready = hooks
                    .bridge
                    .book
                    .lock()
                    .unwrap()
                    .session_mut(ctx.session_id)
                    .unwrap()
                    .emulated()
                    .count()
                    == 2;
                if ready {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(trace.lock().unwrap().feedback.len(), 2);
        assert_eq!(
            trace.lock().unwrap().reports.last().unwrap().buttons,
            0x2000
        );
        assert_eq!(
            hooks
                .bridge
                .book
                .lock()
                .unwrap()
                .session_mut(ctx.session_id)
                .unwrap()
                .emulated()
                .collect::<Vec<_>>(),
            [(7, 0), (9, 1)]
        );
        hooks.bridge.report(9, &PadState::default());
        assert_eq!(trace.lock().unwrap().reports.last().unwrap().buttons, 0);
        hooks.bridge.disconnected(9, &hooks.runtime);
        drops(&trace, 1).await;
        hooks.aborted(&ctx);
        drops(&trace, 2).await;
    }
}
