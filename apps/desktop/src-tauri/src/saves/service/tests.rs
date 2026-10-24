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
use crate::saves::sync::tests::{Rig, serve_blobs, serve_head, snapshot};

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
    saves_with(rig, signed_in, false)
}

fn saves_with(rig: &Rig, signed_in: bool, running: bool) -> (Saves, EventBus) {
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
        Arc::new(move |_| running),
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

mod resolve {
    use crate::error::AppError;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    /// Synced on `synced`, played on here, while the cloud moved to `head`.
    async fn conflicted(rig: &Rig, head: Uuid) -> Uuid {
        install(rig, true).await;
        rig.write("slot.sav", b"synced");
        rig.synced(Uuid::now_v7(), &[("slot.sav", b"synced")]).await;
        rig.write("slot.sav", b"played here");
        let backup = Uuid::now_v7();
        rig.store
            .record_backup(rig.scope, backup, BackupReason::Conflict, 1, 11)
            .await
            .unwrap();
        let cloud = serde_json::from_value(snapshot(
            head,
            rig.scope.package.package_id,
            &[("slot.sav", b"played elsewhere")],
        ))
        .unwrap();
        rig.store
            .open_conflict(rig.scope, cloud, backup)
            .await
            .unwrap()
    }

    async fn serve_cloud(server: &MockServer, rig: &Rig, head: Uuid) {
        let files: &[(&str, &[u8])] = &[("slot.sav", b"played elsewhere")];
        serve_head(
            server,
            rig,
            Some(snapshot(head, rig.scope.package.package_id, files)),
        )
        .await;
        serve_blobs(server, rig, files, false).await;
    }

    /// The server accepts one commit and returns it as `pushed`.
    async fn serve_commit(server: &MockServer, rig: &Rig, pushed: Uuid) {
        let package = rig.scope.package.package_id;
        Mock::given(method("POST"))
            .and(path(format!("/v1/saves/{package}/blobs/prepare")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "missing": [] })))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/v1/saves/{package}/snapshots")))
            .respond_with(ResponseTemplate::new(201).set_body_json(snapshot(
                pushed,
                package,
                &[("slot.sav", b"played here")],
            )))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn commit_parent(server: &MockServer) -> serde_json::Value {
        let requests = server.received_requests().await.unwrap();
        let commit = requests
            .iter()
            .find(|r| r.url.path().ends_with("/snapshots"))
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&commit.body).unwrap()["parent_snapshot_id"]
            .clone()
    }

    #[tokio::test]
    async fn the_dialog_shows_both_sides_and_cancel_changes_nothing() {
        let server = MockServer::start().await;
        let rig = Rig::new(&server.uri()).await;
        let head = Uuid::now_v7();
        let id = conflicted(&rig, head).await;
        let (service, _) = saves(&rig, true);
        let conflict = service.conflict(id).await.unwrap();
        assert_eq!(
            conflict.cloud.source,
            SaveSource::Cloud { snapshot_id: head }
        );
        assert_eq!(conflict.cloud.file_count, 1);
        assert_eq!(conflict.device.total_bytes, 11);
        let status = service.resolve(id, ConflictChoice::Cancel).await.unwrap();
        assert_eq!(status.conflict_id, Some(id));
        assert!(server.received_requests().await.unwrap().is_empty());
        assert_eq!(rig.read("slot.sav").unwrap(), b"played here");
    }

    #[tokio::test]
    async fn keep_cloud_restores_the_head_and_closes_the_conflict() {
        let server = MockServer::start().await;
        let rig = Rig::new(&server.uri()).await;
        let head = Uuid::now_v7();
        let id = conflicted(&rig, head).await;
        serve_cloud(&server, &rig, head).await;
        let (service, _) = saves(&rig, true);
        let status = service
            .resolve(id, ConflictChoice::KeepCloud)
            .await
            .unwrap();
        assert_eq!(status.state, CloudSaveState::Synced);
        assert_eq!(rig.read("slot.sav").unwrap(), b"played elsewhere");
        assert_eq!(
            rig.store.state(rig.scope).await.unwrap().base_snapshot_id,
            Some(head)
        );
        assert!(matches!(
            service.conflict(id).await,
            Err(AppError::NotFound)
        ));
    }

    #[tokio::test]
    async fn keep_device_pushes_over_the_head_it_was_shown() {
        let server = MockServer::start().await;
        let rig = Rig::new(&server.uri()).await;
        let head = Uuid::now_v7();
        let id = conflicted(&rig, head).await;
        let pushed = Uuid::now_v7();
        serve_commit(&server, &rig, pushed).await;
        let (service, _) = saves(&rig, true);
        let status = service
            .resolve(id, ConflictChoice::KeepDevice)
            .await
            .unwrap();
        assert_eq!(status.state, CloudSaveState::Synced);
        assert_eq!(commit_parent(&server).await, json!(head));
        assert_eq!(rig.read("slot.sav").unwrap(), b"played here");
        assert_eq!(
            rig.store.state(rig.scope).await.unwrap().base_snapshot_id,
            Some(pushed)
        );
    }

    #[tokio::test]
    async fn keep_both_backs_up_the_cloud_side_then_pushes_the_device() {
        let server = MockServer::start().await;
        let rig = Rig::new(&server.uri()).await;
        let head = Uuid::now_v7();
        let id = conflicted(&rig, head).await;
        serve_cloud(&server, &rig, head).await;
        serve_commit(&server, &rig, Uuid::now_v7()).await;
        let (service, _) = saves(&rig, true);
        let before = rig.store.backups(rig.scope).await.unwrap().len();
        let status = service.resolve(id, ConflictChoice::KeepBoth).await.unwrap();
        assert_eq!(status.state, CloudSaveState::Synced);
        assert_eq!(commit_parent(&server).await, json!(head));
        let backups = rig.store.backups(rig.scope).await.unwrap();
        assert_eq!(backups.len(), before + 1);
        let kept = backups.iter().max_by_key(|b| b.id).unwrap();
        assert_eq!((kept.file_count, kept.total_bytes), (1, 16));
        assert_eq!(rig.read("slot.sav").unwrap(), b"played here");
    }

    #[tokio::test]
    async fn a_head_that_moved_renews_the_conflict_instead_of_overwriting() {
        let server = MockServer::start().await;
        let rig = Rig::new(&server.uri()).await;
        let id = conflicted(&rig, Uuid::now_v7()).await;
        let newer = Uuid::now_v7();
        serve_cloud(&server, &rig, newer).await;
        let (service, bus) = saves(&rig, true);
        let mut events = bus.subscribe();
        let status = service
            .resolve(id, ConflictChoice::KeepCloud)
            .await
            .unwrap();
        assert_eq!(status.conflict_id, Some(id));
        assert_eq!(rig.read("slot.sav").unwrap(), b"played here");
        let conflict = service.conflict(id).await.unwrap();
        assert_eq!(
            conflict.cloud.source,
            SaveSource::Cloud { snapshot_id: newer }
        );
        let mut renewed = false;
        while let Ok(event) = events.try_recv() {
            renewed |= matches!(event, AppEvent::SaveConflict(c) if c.conflict_id == id);
        }
        assert!(renewed);
    }

    #[tokio::test]
    async fn a_running_game_another_account_or_offline_change_nothing() {
        let rig = Rig::new("http://127.0.0.1:1").await;
        let id = conflicted(&rig, Uuid::now_v7()).await;
        let (running, _) = saves_with(&rig, true, true);
        assert!(matches!(
            running.resolve(id, ConflictChoice::KeepCloud).await,
            Err(AppError::InvalidInput { .. })
        ));
        let (signed_out, _) = saves(&rig, false);
        assert!(matches!(
            signed_out.conflict(id).await,
            Err(AppError::Unauthenticated)
        ));
        let (service, _) = saves(&rig, true);
        assert!(matches!(
            service.resolve(id, ConflictChoice::KeepDevice).await,
            Err(AppError::Network { .. })
        ));
        assert!(service.conflict(id).await.is_ok());
        assert_eq!(rig.read("slot.sav").unwrap(), b"played here");
    }
}

#[tokio::test]
async fn a_launch_during_a_resolution_is_refused_at_once() {
    let rig = Rig::new("http://127.0.0.1:1").await;
    install(&rig, true).await;
    let (service, _) = saves(&rig, true);
    let _busy = service.lock(rig.scope.package).lock_owned().await;
    let mut ctx_saves = ManifestSaves { locations: vec![] };
    ctx_saves.locations.push(SaveLocation {
        id: "game".into(),
        base: SaveBase::Documents,
        path: "game".into(),
        include: vec![],
        exclude: vec![],
    });
    let ctx = LaunchContext {
        package: rig.scope.package,
        root: rig.root.path().join("library/game"),
        platform: Platform::LinuxX86_64,
        layer: LaunchLayer::Native,
        prefix_dir: None,
        saves: Some(Arc::new(ctx_saves)),
        cancellation: CancellationToken::new(),
    };
    let started = std::time::Instant::now();
    assert_eq!(
        service.before_spawn(&ctx).await,
        Err(LaunchVeto::SavesUnavailable)
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}
