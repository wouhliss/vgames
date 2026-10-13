use super::*;
use vgames_proto::saves::OsFamily;

fn scope() -> Scope {
    Scope {
        package: PackageRef {
            server_id: Uuid::now_v7(),
            package_id: Uuid::now_v7(),
        },
        user_id: Uuid::now_v7(),
    }
}
async fn server(db: &Db, scope: Scope) {
    db.call(move |conn| {
        conn.execute("INSERT OR IGNORE INTO servers (id,url,name,root_public_key,root_fingerprint,added_at) VALUES (?1,?2,'Fixture',zeroblob(32),'VG1',0)", params![scope.package.server_id.to_string(), format!("https://{}.example.invalid", scope.package.server_id)])?;
        Ok(())
    }).await.unwrap();
}
fn file() -> SaveFile {
    SaveFile {
        root: "game".into(),
        path: "slot.sav".into(),
        size: 4,
        blake3: blake3::hash(b"save").to_hex().to_string(),
        mtime: time::OffsetDateTime::UNIX_EPOCH,
    }
}
fn request() -> CommitSnapshotRequest {
    CommitSnapshotRequest {
        parent_snapshot_id: None,
        platform: OsFamily::Linux,
        label: None,
        files: vec![file()],
    }
}

#[tokio::test]
async fn retries_survive_restart_and_delayed_ack_cannot_replace_new_payload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite3");
    let scope = scope();
    let first = {
        let db = Db::open(&path).unwrap();
        server(&db, scope).await;
        Store::new(db).begin_push(scope, request()).await.unwrap()
    };
    let store = Store::new(Db::open(&path).unwrap());
    assert_eq!(store.begin_push(scope, request()).await.unwrap(), first);
    let mut changed = request();
    changed.files[0].blake3 = blake3::hash(b"next").to_hex().to_string();
    let next = store.begin_push(scope, changed.clone()).await.unwrap();
    assert_ne!(next.id, first.id);
    let head = Uuid::now_v7();
    assert!(!store.finish_push(scope, first.id, head).await.unwrap());
    let pending = store.state(scope).await.unwrap();
    assert_eq!(pending.push, Some(next.clone()));
    assert_eq!(pending.base_snapshot_id, None);
    assert!(pending.pending);
    assert!(store.finish_push(scope, next.id, head).await.unwrap());
    let state = store.state(scope).await.unwrap();
    assert_eq!(state.base_snapshot_id, Some(head));
    assert_eq!(state.files.get("game/slot.sav"), changed.files.first());
    assert!(state.last_synced_at.is_some());
    assert!(!state.pending);
    assert!(state.push.is_none());
    assert!(
        !store
            .finish_push(scope, next.id, Uuid::now_v7())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn server_account_and_package_are_independent_and_offline_preserves_retry() {
    let db = Db::open_in_memory().unwrap();
    let original = scope();
    let other_account = Scope {
        user_id: Uuid::now_v7(),
        ..original
    };
    let other_package = Scope {
        package: PackageRef {
            package_id: Uuid::now_v7(),
            ..original.package
        },
        ..original
    };
    let other_server = Scope {
        package: PackageRef {
            server_id: Uuid::now_v7(),
            ..original.package
        },
        ..original
    };
    server(&db, original).await;
    server(&db, other_server).await;
    let store = Store::new(db);
    let operation = store.begin_push(original, request()).await.unwrap();
    store.mark_pending(original).await.unwrap();
    assert_eq!(store.state(original).await.unwrap().push, Some(operation));
    for scope in [other_account, other_package, other_server] {
        assert_eq!(store.state(scope).await.unwrap(), SyncState::default());
        let op = store.begin_push(scope, request()).await.unwrap();
        store
            .finish_push(scope, op.id, Uuid::now_v7())
            .await
            .unwrap();
    }
    assert!(store.state(original).await.unwrap().pending);
}

#[test]
fn every_pull_row_content_deletion_mtime_and_first_sync() {
    let head = Uuid::now_v7();
    let files = BTreeMap::from([("game/slot.sav".into(), file())]);
    let base = SyncState {
        base_snapshot_id: Some(head),
        files: files.clone(),
        ..SyncState::default()
    };
    assert_eq!(
        decide_pull(&base, &files, Some(head)),
        PullDecision::Unchanged
    );
    assert_eq!(
        decide_pull(&base, &BTreeMap::new(), Some(head)),
        PullDecision::LocalProgress
    );
    assert_eq!(
        decide_pull(&base, &files, Some(Uuid::now_v7())),
        PullDecision::Restore
    );
    assert_eq!(
        decide_pull(&base, &BTreeMap::new(), Some(Uuid::now_v7())),
        PullDecision::Conflict
    );
    let mut retimed = files.clone();
    retimed.get_mut("game/slot.sav").unwrap().mtime += time::Duration::seconds(10);
    assert_eq!(
        decide_pull(&base, &retimed, Some(head)),
        PullDecision::Unchanged
    );
    assert_eq!(
        decide_pull(&SyncState::default(), &BTreeMap::new(), None),
        PullDecision::Unchanged
    );
    assert_eq!(
        decide_pull(&SyncState::default(), &files, None),
        PullDecision::LocalProgress
    );
    assert_eq!(
        decide_pull(&SyncState::default(), &files, Some(head)),
        PullDecision::Conflict
    );
}

#[tokio::test]
async fn migration_preserves_unattributed_state_without_assigning_it_to_a_login() {
    let mut conn = Connection::open_in_memory().unwrap();
    let migrations = super::super::migrations::MIGRATIONS;
    super::super::migrations::run_list(&mut conn, &migrations[..migrations.len() - 1]).unwrap();
    let scope = scope();
    conn.execute("INSERT INTO servers (id,url,name,root_public_key,root_fingerprint,added_at) VALUES (?1,'https://fixture.example.invalid','Fixture',zeroblob(32),'VG1',0)", [scope.package.server_id.to_string()]).unwrap();
    let head = Uuid::now_v7();
    conn.execute("INSERT INTO save_sync_state (server_id,package_id,base_snapshot_id,updated_at) VALUES (?1,?2,?3,0)", params![scope.package.server_id.to_string(),scope.package.package_id.to_string(),head.to_string()]).unwrap();
    super::super::migrations::run(&mut conn).unwrap();
    assert_eq!(load(&conn, scope).unwrap(), SyncState::default());
    let legacy: String = conn
        .query_row(
            "SELECT base_snapshot_id FROM save_sync_state WHERE user_id=''",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(legacy, head.to_string());
    conn.execute(
        "UPDATE save_sync_state SET user_id=?1, base_snapshot_id='corrupt'",
        [scope.user_id.to_string()],
    )
    .unwrap();
    assert!(matches!(load(&conn, scope), Err(DbError::Json(_))));
}
