use time::OffsetDateTime;
use vgames_proto::saves::OsFamily;

use super::*;
use crate::db::Db;

fn scope() -> Scope {
    Scope {
        package: PackageRef {
            server_id: Uuid::now_v7(),
            package_id: Uuid::now_v7(),
        },
        user_id: Uuid::now_v7(),
    }
}

async fn store(scope: Scope) -> Store {
    let db = Db::open_in_memory().unwrap();
    db.call(move |conn| {
        conn.execute(
            "INSERT INTO servers (id,url,name,root_public_key,root_fingerprint,added_at) VALUES (?1,'https://fixture.example.invalid','Fixture',zeroblob(32),'VG1',0)",
            [scope.package.server_id.to_string()],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    Store::new(db)
}

fn cloud(scope: Scope, files: i32) -> SaveSnapshotSummary {
    SaveSnapshotSummary {
        id: Uuid::now_v7(),
        package_id: scope.package.package_id,
        parent_id: None,
        device_id: None,
        device_name: Some("Desk".into()),
        platform: OsFamily::Windows,
        file_count: files,
        total_size: 10,
        label: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
    }
}

async fn backup(store: &Store, scope: Scope) -> Uuid {
    let id = Uuid::now_v7();
    store
        .record_backup(scope, id, BackupReason::Conflict, 1, 4)
        .await
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    id
}

#[tokio::test]
async fn a_conflict_stays_open_with_one_stable_id_until_resolved_once() {
    let scope = scope();
    let store = store(scope).await;
    let device = backup(&store, scope).await;
    let first = cloud(scope, 1);
    let id = store
        .open_conflict(scope, first.clone(), device)
        .await
        .unwrap();
    let record = store.conflict(id).await.unwrap().unwrap();
    assert_eq!(
        (record.scope, record.cloud, record.device_backup_id),
        (scope, first, device)
    );
    assert_eq!(record.resolved_at, None);

    // Detected again after the head moved: same id, newer cloud side, the
    // original device backup still protects local files.
    let later = cloud(scope, 3);
    let again = store
        .open_conflict(scope, later.clone(), backup(&store, scope).await)
        .await
        .unwrap();
    assert_eq!(again, id);
    let record = store.open_conflict_for(scope).await.unwrap().unwrap();
    assert_eq!(record.cloud, later);
    assert_eq!(record.device_backup_id, device);

    // Another account of the same package cannot see or resolve it.
    let other = Scope {
        user_id: Uuid::now_v7(),
        ..scope
    };
    assert!(!store.resolve_conflict(other, id).await.unwrap());
    assert!(store.open_conflict_for(other).await.unwrap().is_none());
    assert!(store.resolve_conflict(scope, id).await.unwrap());
    assert!(!store.resolve_conflict(scope, id).await.unwrap());
    assert!(store.open_conflict_for(scope).await.unwrap().is_none());
    assert!(
        store
            .conflict(id)
            .await
            .unwrap()
            .unwrap()
            .resolved_at
            .is_some()
    );

    // A later conflict is a new record.
    let next = store
        .open_conflict(scope, cloud(scope, 2), backup(&store, scope).await)
        .await
        .unwrap();
    assert_ne!(next, id);
    assert!(store.conflict(Uuid::now_v7()).await.unwrap().is_none());
}

#[tokio::test]
async fn retention_never_removes_an_open_conflicts_backup() {
    let scope = scope();
    let store = store(scope).await;
    let device = backup(&store, scope).await;
    let id = store
        .open_conflict(scope, cloud(scope, 1), device)
        .await
        .unwrap();
    for _ in 0..6 {
        backup(&store, scope).await;
    }
    let kept = store.retain_backups(scope, 5).await.unwrap();
    assert_eq!(kept.len(), 6);
    assert!(kept.contains(&device));
    assert!(store.conflict(id).await.unwrap().is_some());

    // Resolved: the device backup ages out like any other, and the resolved
    // record goes with it instead of blocking retention.
    assert!(store.resolve_conflict(scope, id).await.unwrap());
    let kept = store.retain_backups(scope, 5).await.unwrap();
    assert_eq!(kept.len(), 5);
    assert!(!kept.contains(&device));
    assert!(store.conflict(id).await.unwrap().is_none());
}

#[tokio::test]
async fn a_cloud_side_of_another_package_is_refused() {
    let scope = scope();
    let store = store(scope).await;
    let device = backup(&store, scope).await;
    let mut wrong = cloud(scope, 1);
    wrong.package_id = Uuid::now_v7();
    assert!(store.open_conflict(scope, wrong, device).await.is_err());
    assert!(store.open_conflict_for(scope).await.unwrap().is_none());
}
