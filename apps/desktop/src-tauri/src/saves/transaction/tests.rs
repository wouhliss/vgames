use std::collections::BTreeMap;

use time::OffsetDateTime;
use vgames_core::manifest::{SaveBase, SaveLocation};

use super::*;
use crate::db::Db;
use crate::db::rusqlite::params;
use crate::db::saves::BackupState;
use crate::events::PackageRef;

fn scope() -> Scope {
    Scope {
        package: PackageRef {
            server_id: Uuid::now_v7(),
            package_id: Uuid::now_v7(),
        },
        user_id: Uuid::now_v7(),
    }
}

async fn store(path: &Path, scope: Scope) -> Store {
    let db = Db::open(path).unwrap();
    db.call(move |conn| {
        conn.execute(
            "INSERT OR IGNORE INTO servers (id,url,name,root_public_key,root_fingerprint,added_at) VALUES (?1,?2,'Fixture',zeroblob(32),'VG1',0)",
            params![
                scope.package.server_id.to_string(),
                format!("https://{}.example.invalid", scope.package.server_id)
            ],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    Store::new(db)
}

fn locations(root: &Path) -> Arc<Vec<Location>> {
    let config = SaveLocation {
        id: "game".into(),
        base: SaveBase::Documents,
        path: "game".into(),
        include: vec![],
        exclude: vec![],
    };
    Arc::new(vec![Location::open(&config, &root.join("docs")).unwrap()])
}

fn file(path: &str, bytes: &[u8]) -> SaveFile {
    SaveFile {
        root: "game".into(),
        path: path.into(),
        size: i64::try_from(bytes.len()).unwrap(),
        blake3: blake3::hash(bytes).to_hex().to_string(),
        mtime: OffsetDateTime::UNIX_EPOCH,
    }
}

fn stage(root: &Path, contents: &[&[u8]]) -> PathBuf {
    let staging = root.join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    for bytes in contents {
        std::fs::write(staging.join(blake3::hash(bytes).to_hex().as_str()), bytes).unwrap();
    }
    staging
}

fn protected() -> Arc<ProtectedInstalls> {
    Arc::new(ProtectedInstalls::open(std::iter::empty()).unwrap())
}

fn request(root: &Path, bytes: &[u8], snapshot: Uuid) -> Request {
    let saved = file("slot.sav", bytes);
    Request {
        writes: vec![saved.clone()],
        deletes: vec![],
        staging: stage(root, &[bytes]),
        reason: BackupReason::Restore,
        base: Some(RestoredBase {
            snapshot_id: Some(snapshot),
            files: BTreeMap::from([("game/slot.sav".to_owned(), saved)]),
        }),
    }
}

fn backup_folders(root: &Path, scope: Scope) -> usize {
    let dir = root
        .join("data/save-backups")
        .join(scope.package.server_id.to_string())
        .join(scope.package.package_id.to_string())
        .join(scope.user_id.to_string());
    std::fs::read_dir(dir).map_or(0, |entries| entries.count())
}

#[tokio::test]
async fn restores_record_the_base_and_keep_five_backups() {
    let root = tempfile::tempdir().unwrap();
    let scope = scope();
    let store = store(&root.path().join("db.sqlite3"), scope).await;
    let txn = Transactions::new(store.clone(), &root.path().join("data"));
    let game = root.path().join("docs/game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join("slot.sav"), b"original").unwrap();
    let locations = locations(root.path());
    let mut last = None;
    for round in 0..7u8 {
        let snapshot = Uuid::now_v7();
        let bytes = [b'0' + round; 8];
        txn.restore(
            scope,
            locations.clone(),
            protected(),
            request(root.path(), &bytes, snapshot),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(game.join("slot.sav")).unwrap(), bytes);
        last = Some(snapshot);
    }
    let state = store.state(scope).await.unwrap();
    assert_eq!(state.base_snapshot_id, last);
    assert!(!state.pending);
    let records = store.backups(scope).await.unwrap();
    assert_eq!(records.len(), restore::KEEP_BACKUPS);
    assert!(
        records
            .iter()
            .all(|record| record.state == BackupState::Complete)
    );
    assert_eq!(backup_folders(root.path(), scope), restore::KEEP_BACKUPS);
}

#[tokio::test]
async fn a_failed_restore_is_undone_and_recorded() {
    let root = tempfile::tempdir().unwrap();
    let scope = scope();
    let store = store(&root.path().join("db.sqlite3"), scope).await;
    let txn = Transactions::new(store.clone(), &root.path().join("data"));
    let game = root.path().join("docs/game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join("slot.sav"), b"original").unwrap();
    std::fs::write(game.join("other.sav"), b"keep me").unwrap();
    // The first write succeeds; the second blob was never staged.
    let mut request = request(root.path(), b"replaced", Uuid::now_v7());
    request.writes.push(file("other.sav", b"never staged"));
    let result = txn
        .restore(
            scope,
            locations(root.path()),
            protected(),
            request,
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(
        result,
        Err(Error::RolledBack(scan::Error::Mismatch))
    ));
    assert_eq!(std::fs::read(game.join("slot.sav")).unwrap(), b"original");
    assert_eq!(std::fs::read(game.join("other.sav")).unwrap(), b"keep me");
    let records = store.backups(scope).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state, BackupState::RolledBack);
    assert_eq!(store.state(scope).await.unwrap().base_snapshot_id, None);
}

#[tokio::test]
async fn an_interrupted_restore_is_rolled_back_on_the_next_use() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("db.sqlite3");
    let scope = scope();
    let game = root.path().join("docs/game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join("slot.sav"), b"original").unwrap();
    // A process that died between applying files and completing the record.
    {
        let store = store(&path, scope).await;
        let txn = Transactions::new(store.clone(), &root.path().join("data"));
        let locations = locations(root.path());
        let staging = stage(root.path(), &[b"half way"]);
        let plan = Plan::new(
            &locations,
            vec![file("slot.sav", b"half way")],
            vec![],
            &protected(),
            &CancellationToken::new(),
        )
        .unwrap();
        let backups = open_dir(&txn.dir(scope), true).unwrap();
        let backup =
            restore::back_up(&locations, &plan, &backups, &CancellationToken::new()).unwrap();
        store
            .begin_restore(scope, backup.id, BackupReason::Restore, 1, 8)
            .await
            .unwrap();
        restore::apply(
            &locations,
            &plan,
            &open_dir(&staging, false).unwrap(),
            &CancellationToken::new(),
        )
        .unwrap();
    }
    assert_eq!(std::fs::read(game.join("slot.sav")).unwrap(), b"half way");
    let store = store(&path, scope).await;
    let txn = Transactions::new(store.clone(), &root.path().join("data"));
    // Another account's use of the same package does not touch this restore.
    let other = Scope {
        user_id: Uuid::now_v7(),
        ..scope
    };
    assert_eq!(txn.recover(other, locations(root.path())).await.unwrap(), 0);
    assert_eq!(txn.recover(scope, locations(root.path())).await.unwrap(), 1);
    assert_eq!(std::fs::read(game.join("slot.sav")).unwrap(), b"original");
    assert!(store.unfinished_restores().await.unwrap().is_empty());
    assert_eq!(txn.recover(scope, locations(root.path())).await.unwrap(), 0);
}

#[tokio::test]
async fn a_missing_backup_keeps_the_restore_unfinished() {
    let root = tempfile::tempdir().unwrap();
    let scope = scope();
    let store = store(&root.path().join("db.sqlite3"), scope).await;
    let txn = Transactions::new(store.clone(), &root.path().join("data"));
    let lost = Uuid::now_v7();
    store
        .begin_restore(scope, lost, BackupReason::Restore, 1, 8)
        .await
        .unwrap();
    assert!(matches!(
        txn.recover(scope, locations(root.path())).await,
        Err(Error::Unrecovered(_))
    ));
    assert_eq!(store.unfinished_restores().await.unwrap(), [(scope, lost)]);
    // A new restore of that package refuses to run over it.
    assert!(matches!(
        txn.restore(
            scope,
            locations(root.path()),
            protected(),
            request(root.path(), b"new", Uuid::now_v7()),
            CancellationToken::new(),
        )
        .await,
        Err(Error::Unrecovered(_))
    ));
}

#[tokio::test]
async fn a_cancelled_restore_changes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let scope = scope();
    let store = store(&root.path().join("db.sqlite3"), scope).await;
    let txn = Transactions::new(store.clone(), &root.path().join("data"));
    let game = root.path().join("docs/game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join("slot.sav"), b"original").unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        txn.restore(
            scope,
            locations(root.path()),
            protected(),
            request(root.path(), b"replaced", Uuid::now_v7()),
            cancel,
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(game.join("slot.sav")).unwrap(), b"original");
    assert!(store.unfinished_restores().await.unwrap().is_empty());
}

#[tokio::test]
async fn protecting_a_conflict_backs_up_the_device_side_without_changing_it() {
    let root = tempfile::tempdir().unwrap();
    let scope = scope();
    let store = store(&root.path().join("db.sqlite3"), scope).await;
    let txn = Transactions::new(store.clone(), &root.path().join("data"));
    let game = root.path().join("docs/game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join("slot.sav"), b"device side").unwrap();
    let id = txn
        .protect(
            scope,
            locations(root.path()),
            protected(),
            vec![file("slot.sav", b"device side")],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(game.join("slot.sav")).unwrap(),
        b"device side"
    );
    let records = store.backups(scope).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        (records[0].id, records[0].reason, records[0].state),
        (id, BackupReason::Conflict, BackupState::Complete)
    );
    assert_eq!((records[0].file_count, records[0].total_bytes), (1, 11));
    // The device side can be put back from that backup later.
    std::fs::write(game.join("slot.sav"), b"cloud side").unwrap();
    let dir = txn.dir(scope);
    let backups = open_dir(&dir, false).unwrap();
    let backup = restore::read_backup(&backups, id).unwrap();
    restore::roll_back(&locations(root.path()), &backup, &backups).unwrap();
    assert_eq!(
        std::fs::read(game.join("slot.sav")).unwrap(),
        b"device side"
    );
    assert!(store.unfinished_restores().await.unwrap().is_empty());
}
