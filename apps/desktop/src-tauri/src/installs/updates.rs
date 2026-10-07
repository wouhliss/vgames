//! Update detection (INS-04), without polling: run at startup, on a server
//! switch or sign-in, when the catalog or a package page is refreshed, and
//! when the main window gains focus (at most once a minute for focus).

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_proto::packages::Platform;

use super::{AvailableUpdate, Installs};
use crate::catalog::{Catalog, Connections};
use crate::db::{self, Db};
use crate::events::{AppEvent, EventBus, InstallsChanged};

/// Focus checks closer together than this are skipped.
const FOCUS_INTERVAL: Duration = Duration::from_secs(60);

/// What the server offers compared with what is installed.
pub fn compare(
    installed_version: Uuid,
    installed_sequence: i64,
    descriptor: &vgames_proto::versions::ReleaseDescriptor,
) -> Option<AvailableUpdate> {
    if descriptor.version_id == installed_version {
        return None;
    }
    let installed_yanked = descriptor.yanked_version_ids.contains(&installed_version);
    if descriptor.sequence <= installed_sequence && !installed_yanked {
        return None;
    }
    Some(AvailableUpdate {
        version_label: descriptor.version_label.clone(),
        sequence: u64::try_from(descriptor.sequence).unwrap_or(0),
        // An upper bound: the update itself fetches only changed chunks.
        download_bytes: u64::try_from(descriptor.total_size).unwrap_or(0),
        installed_yanked,
    })
}

/// Checks every installed package of the active server. Returns how many
/// have an update.
pub async fn detect<C: Connections>(catalog: &Catalog<C>, db: &Db, installs: &Installs) -> usize {
    let Ok(server_id) = catalog.active_server().await else {
        return 0;
    };
    let rows = match db::installs::list(db, server_id).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "cannot read the installs for update detection");
            return 0;
        }
    };
    let mut found = 0;
    for row in rows.into_iter().filter(|r| r.state == "installed") {
        let (Some(platform), Ok(version)) = (
            Platform::parse(&row.platform),
            Uuid::parse_str(&row.version_id),
        ) else {
            continue;
        };
        match catalog.descriptor(row.package, platform).await {
            Ok(descriptor) => {
                let update = compare(version, row.sequence, &descriptor);
                found += usize::from(update.is_some());
                installs.set_update(row.package, update);
            }
            // Gone or offline: keep what was known.
            Err(error) => {
                tracing::debug!(%error, package = %row.package.package_id, "no release information")
            }
        }
    }
    found
}

/// Wakes the detection task.
#[derive(Default)]
pub struct UpdateTriggers {
    /// The main window gained focus (rate-limited).
    pub focus: tokio::sync::Notify,
    /// The catalog or a package page was refreshed.
    pub refresh: tokio::sync::Notify,
}

/// Runs detection on the events that call for it, until `stop`.
pub fn spawn<C: Connections>(
    catalog: Arc<Catalog<C>>,
    db: Db,
    installs: Arc<Installs>,
    bus: EventBus,
    triggers: Arc<UpdateTriggers>,
    stop: CancellationToken,
) {
    let mut events = bus.subscribe();
    tauri::async_runtime::spawn(async move {
        let mut last_focus: Option<Instant> = None;
        let run = |reason: &'static str| {
            let (catalog, db, installs, bus) = (&catalog, &db, &installs, &bus);
            async move {
                let found = detect(catalog, db, installs).await;
                tracing::debug!(reason, found, "update detection");
                bus.publish(AppEvent::InstallsChanged(InstallsChanged {}));
            }
        };
        run("startup").await;
        loop {
            tokio::select! {
                () = stop.cancelled() => return,
                () = triggers.refresh.notified() => run("catalog").await,
                () = triggers.focus.notified() => {
                    if last_focus.is_some_and(|at| at.elapsed() < FOCUS_INTERVAL) {
                        continue;
                    }
                    last_focus = Some(Instant::now());
                    run("focus").await;
                }
                event = events.recv() => match event {
                    Ok(AppEvent::ServerSwitched(_) | AppEvent::AuthFinished(_)) => run("server").await,
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => run("lagged").await,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(
        version: u128,
        sequence: i64,
        yanked: &[u128],
    ) -> vgames_proto::versions::ReleaseDescriptor {
        serde_json::from_value(serde_json::json!({
            "package_id": Uuid::from_u128(1),
            "version_id": Uuid::from_u128(version),
            "platform": "linux-x86_64",
            "sequence": sequence,
            "version_label": format!("1.{sequence}"),
            "total_size": 1000,
            "pack_count": 1,
            "manifest": {"url": "https://s/m", "size": 1, "blake3": "0".repeat(64), "expires_at": "2026-10-06T00:00:00Z"},
            "signature": {"format": "vgames.sig/1", "alg": "ed25519", "context": "vgames/manifest/v1",
                          "key_id": "0".repeat(32), "payload_blake3": "0".repeat(64), "signature": "AA=="},
            "yanked_version_ids": yanked.iter().map(|v| Uuid::from_u128(*v)).collect::<Vec<_>>(),
            "published_at": "2026-10-06T00:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn newer_releases_and_yanked_installs_are_offered() {
        let installed = Uuid::from_u128(10);
        assert_eq!(compare(installed, 3, &descriptor(10, 3, &[])), None);
        let update = compare(installed, 3, &descriptor(11, 4, &[])).unwrap();
        assert_eq!((update.sequence, update.installed_yanked), (4, false));
        // An older release is never offered, unless the installed one was withdrawn.
        assert_eq!(compare(installed, 3, &descriptor(9, 2, &[])), None);
        let back = compare(installed, 3, &descriptor(9, 2, &[10])).unwrap();
        assert!(back.installed_yanked);
    }
}
