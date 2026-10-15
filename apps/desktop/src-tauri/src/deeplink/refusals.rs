//! Fixed refusal classifications; no original URL or server message reaches UI.
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use tokio_util::sync::CancellationToken;
use vgames_proto::packages::PackageDetail;

use crate::{
    api::ApiError,
    events::{AppEvent, DeeplinkRefusalReason as Reason, DeeplinkRefused, EventBus, PackageRef},
    launch::orchestrate::LaunchError,
    servers::Servers,
};

pub(super) struct Refusals {
    bus: EventBus,
    last: Mutex<Option<Instant>>,
    ready: CancellationToken,
    shutdown: CancellationToken,
}
impl Refusals {
    pub fn new(bus: EventBus, ready: CancellationToken, shutdown: CancellationToken) -> Self {
        Self {
            bus,
            last: Mutex::new(None),
            ready,
            shutdown,
        }
    }
    pub async fn notify(&self, reason: Reason) {
        // The guard is dropped before waiting; UI readiness never holds a lock.
        {
            let Ok(mut last) = self.last.lock() else {
                return;
            };
            let now = Instant::now();
            if last.is_some_and(|previous| now.duration_since(previous) < Duration::from_secs(1)) {
                return;
            }
            *last = Some(now);
        }
        tokio::select! {
            biased;
            () = self.shutdown.cancelled() => {},
            () = self.ready.cancelled() => self.bus.publish(AppEvent::DeeplinkRefused(DeeplinkRefused { reason })),
        }
    }
}

pub(super) fn local_reason(error: &LaunchError) -> Option<Reason> {
    match error {
        LaunchError::RateLimited => Some(Reason::RateLimited),
        LaunchError::NotInstalled => Some(Reason::NotInstalled),
        LaunchError::Incomplete | LaunchError::Integrity { .. } | LaunchError::KeyRevoked => {
            Some(Reason::NotVerified)
        }
        _ => None,
    }
}
fn catalog_reason(result: Result<PackageDetail, ApiError>) -> Reason {
    match result {
        Err(ApiError::Problem { status: 404, .. }) => Reason::UnknownPackage,
        _ => Reason::NotInstalled,
    }
}

/// Only an authenticated 404 proves absence. Timeouts, offline, authentication
/// failures and malformed replies cannot change the known local install state.
pub(super) async fn missing_reason(
    servers: &Servers,
    package: PackageRef,
    shutdown: &CancellationToken,
) -> Reason {
    let lookup = async {
        let client = servers.api(package.server_id).await.ok()?;
        Some(catalog_reason(
            client
                .authed::<(), PackageDetail>(
                    reqwest::Method::GET,
                    &format!("v1/packages/{}", package.package_id),
                    None,
                )
                .await,
        ))
    };
    tokio::select! {
        biased;
        () = shutdown.cancelled() => Reason::NotInstalled,
        result = tokio::time::timeout(Duration::from_secs(5), lookup) => result.ok().flatten().unwrap_or(Reason::NotInstalled),
    }
}

#[cfg(test)]
mod tests;
