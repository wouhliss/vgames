use super::*;
use crate::launch::{ProtonPlan, WinePlan};
use vgames_core::compat::Graphics;
use vgames_pack::Compression;
use vgames_transfer::testkit::{FileSpec, TestPackage};

type Calls = Arc<Mutex<Vec<(&'static str, &'static str)>>>;

#[derive(Clone, Copy)]
enum Behavior {
    Ok,
    Veto,
    Pending,
    PanicCall,
    PanicPoll,
}

struct Hook {
    name: &'static str,
    behavior: Behavior,
    calls: Calls,
}
impl LaunchHooks for Hook {
    fn before_spawn<'a>(
        &'a self,
        _: &'a LaunchContext,
    ) -> HookFuture<'a, Result<HookEnv, LaunchVeto>> {
        self.calls.lock().unwrap().push((self.name, "before"));
        if matches!(self.behavior, Behavior::PanicCall) {
            panic!("future construction");
        }
        Box::pin(async {
            match self.behavior {
                Behavior::Ok => Ok(HookEnv(vec![(self.name.into(), "yes".into())])),
                Behavior::Veto => Err(LaunchVeto::SaveConflict {
                    conflict_id: "conflict-1".into(),
                }),
                Behavior::Pending => std::future::pending().await,
                Behavior::PanicPoll => panic!("future poll"),
                Behavior::PanicCall => unreachable!(),
            }
        })
    }
    fn aborted(&self, _: &LaunchContext) {
        self.calls.lock().unwrap().push((self.name, "aborted"));
    }
    fn after_exit<'a>(&'a self, _: &'a LaunchContext, _: &'a GameExit) -> HookFuture<'a, ()> {
        Box::pin(async {
            self.calls.lock().unwrap().push((self.name, "exit"));
        })
    }
    fn before_spawn_budget(&self) -> Duration {
        Duration::from_millis(10)
    }
}

fn register(
    registry: &HookRegistry,
    calls: &Calls,
    stage: HookStage,
    name: &'static str,
    behavior: Behavior,
) {
    assert!(registry.register(
        stage,
        Arc::new(Hook {
            name,
            behavior,
            calls: calls.clone()
        })
    ));
}

fn context() -> LaunchContext {
    LaunchContext {
        session_id: uuid::Uuid::new_v4(),
        controllers: None,
        package: PackageRef {
            server_id: uuid::Uuid::from_u128(1),
            package_id: uuid::Uuid::from_u128(2),
        },
        root: PathBuf::from("game"),
        platform: Platform::WindowsX86_64,
        layer: LaunchLayer::Proton,
        prefix_dir: Some(PathBuf::from("data/prefixes/1/2")),
        cancellation: CancellationToken::new(),
    }
}

fn manifest() -> Manifest {
    let package = TestPackage::build(&[FileSpec::random("data", 32, 1)], &[], Compression::None);
    vgames_core::manifest::parse_and_validate(&package.manifest).unwrap()
}

#[tokio::test]
async fn fixed_order_and_successful_exit() {
    let registry = HookRegistry::default();
    let calls = Calls::default();
    register(
        &registry,
        &calls,
        HookStage::Overlay,
        "overlay",
        Behavior::Ok,
    );
    register(
        &registry,
        &calls,
        HookStage::Controllers,
        "controllers",
        Behavior::Ok,
    );
    register(&registry, &calls, HookStage::Saves, "saves", Behavior::Ok);
    assert!(!registry.register(
        HookStage::Saves,
        Arc::new(Hook {
            name: "duplicate",
            behavior: Behavior::Ok,
            calls: calls.clone()
        })
    ));
    let mut session = registry.session(context());
    assert_eq!(session.before_spawn().await.unwrap().0.len(), 3);
    session.started();
    session
        .after_exit(&GameExit {
            code: Some(0),
            stopped_by_user: false,
            session_seconds: 1,
        })
        .await;
    drop(session);
    assert_eq!(
        *calls.lock().unwrap(),
        [
            ("saves", "before"),
            ("controllers", "before"),
            ("overlay", "before"),
            ("saves", "exit"),
            ("controllers", "exit"),
            ("overlay", "exit")
        ]
    );
}

#[tokio::test]
async fn veto_aborts_previous_hooks_and_never_calls_later_hooks() {
    let registry = HookRegistry::default();
    let calls = Calls::default();
    register(&registry, &calls, HookStage::Saves, "saves", Behavior::Ok);
    register(
        &registry,
        &calls,
        HookStage::Controllers,
        "veto",
        Behavior::Veto,
    );
    register(
        &registry,
        &calls,
        HookStage::Overlay,
        "overlay",
        Behavior::Ok,
    );
    let mut session = registry.session(context());
    assert_eq!(
        session.before_spawn().await,
        Err(LaunchError::SaveConflict {
            conflict_id: "conflict-1".into()
        })
    );
    drop(session);
    assert_eq!(
        *calls.lock().unwrap(),
        [
            ("saves", "before"),
            ("veto", "before"),
            ("veto", "aborted"),
            ("saves", "aborted")
        ]
    );
    assert_eq!(
        serde_json::to_value(LaunchError::SaveConflict {
            conflict_id: "id".into()
        })
        .unwrap()["kind"],
        "save_conflict"
    );
}

#[tokio::test]
async fn timeout_and_both_kinds_of_panics_skip_only_the_failing_hook() {
    for behavior in [Behavior::Pending, Behavior::PanicCall, Behavior::PanicPoll] {
        let registry = HookRegistry::default();
        let calls = Calls::default();
        register(&registry, &calls, HookStage::Saves, "bad", behavior);
        register(
            &registry,
            &calls,
            HookStage::Overlay,
            "overlay",
            Behavior::Ok,
        );
        let mut session = registry.session(context());
        assert_eq!(
            session.before_spawn().await.unwrap(),
            HookEnv(vec![("overlay".into(), "yes".into())])
        );
        session.started();
        session
            .after_exit(&GameExit {
                code: None,
                stopped_by_user: false,
                session_seconds: 0,
            })
            .await;
        assert_eq!(
            *calls.lock().unwrap(),
            [
                ("bad", "before"),
                ("bad", "aborted"),
                ("overlay", "before"),
                ("overlay", "exit")
            ]
        );
    }
}

#[tokio::test]
async fn dropping_preparation_releases_partial_hooks_in_reverse_order() {
    let registry = HookRegistry::default();
    let calls = Calls::default();
    register(&registry, &calls, HookStage::Saves, "saves", Behavior::Ok);
    register(
        &registry,
        &calls,
        HookStage::Controllers,
        "controllers",
        Behavior::Ok,
    );
    let mut session = registry.session(context());
    session.before_spawn().await.unwrap();
    drop(session);
    assert_eq!(
        *calls.lock().unwrap(),
        [
            ("saves", "before"),
            ("controllers", "before"),
            ("controllers", "aborted"),
            ("saves", "aborted")
        ]
    );
}

struct Source {
    result: Result<LaunchPlan, LaunchError>,
    calls: Calls,
}
impl PlanSource for Source {
    fn plan<'a>(
        &'a self,
        _: &'a LaunchContext,
        _: &'a Manifest,
    ) -> HookFuture<'a, Result<LaunchPlan, LaunchError>> {
        Box::pin(async {
            self.calls.lock().unwrap().push(("plan", "prepare"));
            self.result.clone()
        })
    }
}

fn proton(ctx: &LaunchContext) -> LaunchPlan {
    LaunchPlan::Proton(ProtonPlan {
        umu_run: "umu-run".into(),
        proton_path: "proton".into(),
        prefix: ctx.prefix_dir.clone().unwrap(),
        umu_game_id: None,
        store: "none".into(),
        env: BTreeMap::new(),
        dll_overrides: String::new(),
    })
}

#[tokio::test]
async fn source_runs_before_hooks_and_compat_never_falls_back_to_native() {
    let mut ctx = context();
    for layer in [LaunchLayer::Proton, LaunchLayer::Wine] {
        ctx.layer = layer;
        let plan = if layer == LaunchLayer::Proton {
            proton(&ctx)
        } else {
            LaunchPlan::Wine(WinePlan {
                wine: "wine".into(),
                prefix: ctx.prefix_dir.clone().unwrap(),
                graphics: Graphics::Dxmt,
                env: BTreeMap::new(),
                dll_overrides: String::new(),
            })
        };
        let registry = HookRegistry::default();
        let calls = Calls::default();
        register(&registry, &calls, HookStage::Saves, "saves", Behavior::Ok);
        assert!(registry.register_plan_source(Arc::new(Source {
            result: Ok(plan.clone()),
            calls: calls.clone()
        })));
        assert_eq!(registry.plan(&ctx, &manifest()).await.unwrap(), plan);
        registry.session(ctx.clone()).before_spawn().await.unwrap();
        assert_eq!(calls.lock().unwrap().first(), Some(&("plan", "prepare")));
    }
    let refused = LaunchError::CompatUnavailable {
        detail: "Game cannot run on this Mac.".into(),
    };
    for result in [Ok(LaunchPlan::Native), Err(refused.clone())] {
        let registry = HookRegistry::default();
        registry.register_plan_source(Arc::new(Source {
            result: result.clone(),
            calls: Calls::default(),
        }));
        let error = registry.plan(&ctx, &manifest()).await.unwrap_err();
        assert!(matches!(error, LaunchError::CompatUnavailable { .. }));
        if result.is_err() {
            assert_eq!(error, refused);
        }
    }
    assert!(matches!(
        HookRegistry::default().plan(&ctx, &manifest()).await,
        Err(LaunchError::CompatUnavailable { .. })
    ));
    ctx.layer = LaunchLayer::Native;
    assert_eq!(
        HookRegistry::default()
            .plan(&ctx, &manifest())
            .await
            .unwrap(),
        LaunchPlan::Native
    );
}

struct BrokenSource(bool);
impl PlanSource for BrokenSource {
    fn plan<'a>(
        &'a self,
        _: &'a LaunchContext,
        _: &'a Manifest,
    ) -> HookFuture<'a, Result<LaunchPlan, LaunchError>> {
        Box::pin(async move {
            if self.0 {
                panic!("plan panic");
            }
            std::future::pending().await
        })
    }
}

#[tokio::test]
async fn source_panic_and_cancellation_fail_closed_without_a_timeout_budget() {
    let registry = HookRegistry::default();
    registry.register_plan_source(Arc::new(BrokenSource(true)));
    assert!(matches!(
        registry.plan(&context(), &manifest()).await,
        Err(LaunchError::CompatUnavailable { .. })
    ));
    let registry = HookRegistry::default();
    registry.register_plan_source(Arc::new(BrokenSource(false)));
    let ctx = context();
    let cancel = ctx.cancellation.clone();
    let source_manifest = manifest();
    let work = registry.plan(&ctx, &source_manifest);
    let cancellation = async {
        tokio::task::yield_now().await;
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(work, cancellation);
    assert!(matches!(result, Err(LaunchError::CompatUnavailable { .. })));
}

#[tokio::test]
async fn cancellation_unwinds_a_hook_while_it_is_pending() {
    let registry = HookRegistry::default();
    let calls = Calls::default();
    register(
        &registry,
        &calls,
        HookStage::Saves,
        "pending",
        Behavior::Pending,
    );
    let ctx = context();
    let cancel = ctx.cancellation.clone();
    let mut session = registry.session(ctx);
    let (result, ()) = tokio::join!(session.before_spawn(), async {
        tokio::task::yield_now().await;
        cancel.cancel();
    });
    assert!(matches!(result, Err(LaunchError::CompatUnavailable { .. })));
    drop(session);
    assert_eq!(
        *calls.lock().unwrap(),
        [("pending", "before"), ("pending", "aborted")]
    );
}

struct PanickingCleanup;
impl LaunchHooks for PanickingCleanup {
    fn before_spawn<'a>(
        &'a self,
        _: &'a LaunchContext,
    ) -> HookFuture<'a, Result<HookEnv, LaunchVeto>> {
        Box::pin(async { Ok(HookEnv::default()) })
    }
    fn aborted(&self, _: &LaunchContext) {
        panic!("cleanup panic");
    }
}

#[tokio::test]
async fn a_cleanup_panic_cannot_prevent_earlier_hooks_from_aborting() {
    let registry = HookRegistry::default();
    let calls = Calls::default();
    register(&registry, &calls, HookStage::Saves, "saves", Behavior::Ok);
    registry.register(HookStage::Overlay, Arc::new(PanickingCleanup));
    let mut session = registry.session(context());
    session.before_spawn().await.unwrap();
    drop(session);
    assert_eq!(
        *calls.lock().unwrap(),
        [("saves", "before"), ("saves", "aborted")]
    );
}

struct BrokenExit(bool);
impl LaunchHooks for BrokenExit {
    fn before_spawn<'a>(
        &'a self,
        _: &'a LaunchContext,
    ) -> HookFuture<'a, Result<HookEnv, LaunchVeto>> {
        Box::pin(async { Ok(HookEnv::default()) })
    }
    fn after_exit<'a>(&'a self, _: &'a LaunchContext, _: &'a GameExit) -> HookFuture<'a, ()> {
        Box::pin(async move {
            if self.0 {
                panic!("exit panic");
            }
            std::future::pending().await
        })
    }
    fn after_exit_budget(&self) -> Duration {
        Duration::from_millis(10)
    }
    fn aborted(&self, _: &LaunchContext) {}
}

#[tokio::test]
async fn failed_exit_hooks_cannot_prevent_later_cleanup() {
    for panic in [false, true] {
        let registry = HookRegistry::default();
        let calls = Calls::default();
        registry.register(HookStage::Saves, Arc::new(BrokenExit(panic)));
        register(
            &registry,
            &calls,
            HookStage::Overlay,
            "overlay",
            Behavior::Ok,
        );
        let mut session = registry.session(context());
        session.before_spawn().await.unwrap();
        session.started();
        session
            .after_exit(&GameExit {
                code: Some(0),
                stopped_by_user: false,
                session_seconds: 1,
            })
            .await;
        assert_eq!(
            *calls.lock().unwrap(),
            [("overlay", "before"), ("overlay", "exit")]
        );
    }
}

#[tokio::test]
async fn incompatible_runner_prefix_and_unexpected_source_errors_fail_closed() {
    let ctx = context();
    let wrong_runner = LaunchPlan::Wine(WinePlan {
        wine: "wine".into(),
        prefix: ctx.prefix_dir.clone().unwrap(),
        graphics: Graphics::Dxmt,
        env: BTreeMap::new(),
        dll_overrides: String::new(),
    });
    let mut wrong_prefix = proton(&ctx);
    if let LaunchPlan::Proton(plan) = &mut wrong_prefix {
        plan.prefix = PathBuf::from("other-prefix");
    }
    for result in [
        Ok(wrong_runner),
        Ok(wrong_prefix),
        Err(LaunchError::Io {
            detail: "setup failed".into(),
        }),
    ] {
        let registry = HookRegistry::default();
        registry.register_plan_source(Arc::new(Source {
            result,
            calls: Calls::default(),
        }));
        assert!(matches!(
            registry.plan(&ctx, &manifest()).await,
            Err(LaunchError::CompatUnavailable { .. })
        ));
    }
}
