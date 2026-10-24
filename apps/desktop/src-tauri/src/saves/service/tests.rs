use std::sync::Arc;

use time::OffsetDateTime;
use vgames_core::manifest::{Platform, SaveBase, SaveLocation, Saves as ManifestSaves};
use vgames_pack::manifest::{Execution, SaveLocation as PackLocation, Saves as PackSaves};
use vgames_proto::saves::{OsFamily, SaveSnapshotSummary};
use vgames_transfer::testkit::{FileSpec, Identity, TestPackage};

use super::*;
use crate::api::ApiClient;
use crate::db::saves::BackupReason;
use crate::launch::hooks::LaunchLayer;
use crate::saves::hook::Account;
use crate::saves::sync::tests::Rig;

struct Fixed(Option<(Uuid, ApiClient)>);

impl Accounts for Fixed {
    fn account<'a>(
        &'a self,
        _server: Uuid,
        cancel: CancellationToken,
    ) -> HookFuture<'a, Option<Account>> {
        let account = self.0.clone().map(|(user_id, api)| Account {
            user_id,
            http: crate::saves::http::Client::new(api, cancel),
            plain_http: true,
        });
        Box::pin(async move { account })
    }
}

/// Registers the rig's package as installed, its manifest declaring `saves`.
async fn install(rig: &Rig, saves: bool) {
    let execution = Execution {
        saves: saves.then(|| PackSaves {
            locations: vec![PackLocation {
                id: "game".into(),
                base: "documents".into(),
                path: "game".into(),
                include: vec![],
                exclude: vec![],
            }],
        }),
        ..Execution::default()
    };
    let package = TestPackage::build_executable(
        &[FileSpec::random("game.bin", 64, 1)],
        &[],
        vgames_pack::Compression::None,
        vgames_pack::PACK_SIZE,
        &Identity {
            platform: Platform::LinuxX86_64,
            ..Identity::default()
        },
        &execution,
    );
    let library = rig.root.path().join("library");
    let meta = library
        .join("game")
        .join(vgames_transfer::install::META_DIR);
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::write(
        meta.join(vgames_transfer::install::MANIFEST_FILE),
        &package.manifest,
    )
    .unwrap();
    let (scope, path) = (rig.scope, library.to_string_lossy().into_owned());
    rig.db
        .call(move |conn| {
            conn.execute(
                "INSERT INTO libraries (id, path, label, created_at) VALUES ('lib', ?1, 'Games', 0)",
                [path],
            )?;
            conn.execute(
                "INSERT INTO installs (server_id, package_id, library_id, dir_name, version_id,
                 sequence, platform, state)
                 VALUES (?1, ?2, 'lib', 'game', ?3, 1, 'linux-x86_64', 'installed')",
                [
                    scope.package.server_id.to_string(),
                    scope.package.package_id.to_string(),
                    Uuid::now_v7().to_string(),
                ],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

fn saves(rig: &Rig, signed_in: bool) -> (Saves, EventBus) {
    std::fs::create_dir_all(rig.root.path().join("docs")).unwrap();
    let bus = EventBus::new();
    let account = signed_in.then(|| (rig.scope.user_id, rig.api.clone()));
    let saves = Saves::new(
        Arc::new(Fixed(account)),
        rig.db.clone(),
        bus.clone(),
        KnownFolders {
            documents: Some(rig.root.path().join("docs")),
            ..KnownFolders::default()
        },
        rig.root.path().join("data"),
    )
    .unwrap();
    (saves, bus)
}

async fn state(saves: &Saves, rig: &Rig) -> SaveStatus {
    saves.status(rig.scope.package).await.unwrap()
}

#[tokio::test]
async fn packages_without_declared_saves_are_unsupported() {
    let rig = Rig::new("http://127.0.0.1:1").await;
    let (service, _) = saves(&rig, true);
    // Not installed.
    assert_eq!(
        state(&service, &rig).await.state,
        CloudSaveState::Unsupported
    );
    install(&rig, false).await;
    let status = state(&service, &rig).await;
    assert_eq!(status.state, CloudSaveState::Unsupported);
    assert_eq!((status.last_synced_at, status.conflict_id), (None, None));
}

#[tokio::test]
async fn status_follows_durable_state_and_the_account() {
    let rig = Rig::new("http://127.0.0.1:1").await;
    install(&rig, true).await;
    let (service, _) = saves(&rig, true);
    // Never synced on this device: work remains.
    assert_eq!(state(&service, &rig).await.state, CloudSaveState::Pending);

    rig.synced(Uuid::now_v7(), &[("slot.sav", b"synced")]).await;
    let synced = state(&service, &rig).await;
    assert_eq!(synced.state, CloudSaveState::Synced);
    assert!(synced.last_synced_at.is_some());

    rig.store.mark_pending(rig.scope).await.unwrap();
    assert_eq!(state(&service, &rig).await.state, CloudSaveState::Pending);

    let backup = Uuid::now_v7();
    rig.store
        .record_backup(rig.scope, backup, BackupReason::Conflict, 0, 0)
        .await
        .unwrap();
    let cloud = SaveSnapshotSummary {
        id: Uuid::now_v7(),
        package_id: rig.scope.package.package_id,
        parent_id: None,
        device_id: None,
        device_name: None,
        platform: OsFamily::Linux,
        file_count: 1,
        total_size: 1,
        label: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
    };
    let id = rig
        .store
        .open_conflict(rig.scope, cloud, backup)
        .await
        .unwrap();
    let conflict = state(&service, &rig).await;
    assert_eq!(conflict.state, CloudSaveState::Conflict);
    assert_eq!(conflict.conflict_id, Some(id));
    // The installs list reads the same state from its blocking pool.
    let service = Arc::new(service);
    let provider = Arc::clone(&service);
    let package = rig.scope.package;
    let listed = tokio::task::spawn_blocking(move || provider.state(package))
        .await
        .unwrap();
    assert_eq!(listed, CloudSaveState::Conflict);

    // Signed out: another account's records are never shown.
    let (signed_out, _) = saves(&rig, false);
    let status = state(&signed_out, &rig).await;
    assert_eq!(status.state, CloudSaveState::Pending);
    assert_eq!(status.conflict_id, None);
}

#[tokio::test]
async fn a_launch_announces_syncing_then_the_outcome() {
    let rig = Rig::new("http://127.0.0.1:1").await;
    install(&rig, true).await;
    rig.synced(Uuid::now_v7(), &[]).await;
    let (service, bus) = saves(&rig, true);
    let mut events = bus.subscribe();
    let ctx = LaunchContext {
        package: rig.scope.package,
        root: rig.root.path().join("library/game"),
        platform: Platform::LinuxX86_64,
        layer: LaunchLayer::Native,
        prefix_dir: None,
        saves: Some(Arc::new(ManifestSaves {
            locations: vec![SaveLocation {
                id: "game".into(),
                base: SaveBase::Documents,
                path: "game".into(),
                include: vec![],
                exclude: vec![],
            }],
        })),
        cancellation: CancellationToken::new(),
    };
    // The server is unreachable: the launch goes ahead, sync pending.
    assert!(service.before_spawn(&ctx).await.is_ok());
    let mut seen = Vec::new();
    while let Ok(AppEvent::SaveSyncChanged(SaveSyncChanged(status))) = events.try_recv() {
        seen.push(status.state);
    }
    assert_eq!(seen, [CloudSaveState::Syncing, CloudSaveState::Pending]);
    assert!(!service.is_syncing(rig.scope.package));
}
