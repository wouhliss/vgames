use super::*;

#[tokio::test]
async fn each_reason_emits_only_its_fixed_wire_value() {
    for (reason, wire) in [
        (Reason::RateLimited, "rate_limited"),
        (Reason::NotInstalled, "not_installed"),
        (Reason::NotVerified, "not_verified"),
        (Reason::UnknownPackage, "unknown_package"),
    ] {
        let bus = EventBus::new();
        let mut receiver = bus.subscribe();
        let ready = CancellationToken::new();
        ready.cancel();
        let refusals = Refusals::new(bus, ready, CancellationToken::new());
        refusals.notify(reason).await;
        let AppEvent::DeeplinkRefused(event) = receiver.try_recv().unwrap() else {
            panic!("wrong event")
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({"reason":wire})
        );
    }
}

#[tokio::test]
async fn bursts_are_throttled_across_reasons_and_shutdown_suppresses_pending_notice() {
    let bus = EventBus::new();
    let mut receiver = bus.subscribe();
    let ready = CancellationToken::new();
    ready.cancel();
    let refusals = Refusals::new(bus.clone(), ready, CancellationToken::new());
    for _ in 0..100 {
        refusals.notify(Reason::RateLimited).await;
        refusals.notify(Reason::NotVerified).await;
    }
    assert!(receiver.try_recv().is_ok());
    assert!(receiver.try_recv().is_err());
    let shutdown = CancellationToken::new();
    let pending = Refusals::new(bus, CancellationToken::new(), shutdown.clone());
    tokio::join!(pending.notify(Reason::UnknownPackage), async {
        tokio::task::yield_now().await;
        shutdown.cancel();
    });
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn initial_notice_waits_for_ui_listeners() {
    let bus = EventBus::new();
    let mut receiver = bus.subscribe();
    let ready = CancellationToken::new();
    let pending = Refusals::new(bus, ready.clone(), CancellationToken::new());
    tokio::join!(pending.notify(Reason::NotInstalled), async {
        tokio::task::yield_now().await;
        assert!(receiver.try_recv().is_err());
        ready.cancel();
    });
    assert!(receiver.try_recv().is_ok());
}

#[test]
fn unrelated_launch_errors_are_not_mislabeled_as_verification_failures() {
    assert_eq!(
        local_reason(&LaunchError::RateLimited),
        Some(Reason::RateLimited)
    );
    assert_eq!(
        local_reason(&LaunchError::NotInstalled),
        Some(Reason::NotInstalled)
    );
    for error in [
        LaunchError::Incomplete,
        LaunchError::Integrity {
            path: "private".into(),
        },
        LaunchError::KeyRevoked,
    ] {
        assert_eq!(local_reason(&error), Some(Reason::NotVerified));
    }
    for error in [
        LaunchError::AlreadyRunning,
        LaunchError::TargetNotFound,
        LaunchError::Io {
            detail: "private".into(),
        },
    ] {
        assert_eq!(local_reason(&error), None);
    }
    for error in [
        ApiError::Timeout,
        ApiError::Unauthenticated,
        ApiError::Network("private".into()),
        ApiError::InvalidResponse("private".into()),
    ] {
        assert_eq!(catalog_reason(Err(error)), Reason::NotInstalled);
    }
    for status in [401, 404, 500] {
        let error = ApiError::Problem {
            status,
            code: "fixture".into(),
            message: "private".into(),
            problem: Box::default(),
        };
        assert_eq!(
            catalog_reason(Err(error)),
            if status == 404 {
                Reason::UnknownPackage
            } else {
                Reason::NotInstalled
            }
        );
    }
}
