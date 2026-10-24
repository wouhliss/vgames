use std::path::Path;

use serde_json::json;
use vgames_core::manifest::{SaveBase, SaveLocation};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::api::{ApiClient, Session};
use crate::db::Db;
use crate::db::rusqlite::params;
use crate::events::PackageRef;
use crate::secrets::{MemoryVault, VaultHandle};

struct Rig {
    root: tempfile::TempDir,
    scope: Scope,
    store: Store,
    sync: Sync,
}

impl Rig {
    async fn new(base: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let scope = Scope {
            package: PackageRef {
                server_id: Uuid::now_v7(),
                package_id: Uuid::now_v7(),
            },
            user_id: Uuid::now_v7(),
        };
        let db = Db::open(&root.path().join("db.sqlite3")).unwrap();
        db.call(move |conn| {
            conn.execute(
                "INSERT INTO servers (id,url,name,root_public_key,root_fingerprint,added_at) VALUES (?1,'https://fixture.example.invalid','Fixture',zeroblob(32),'VG1',0)",
                params![scope.package.server_id.to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let store = Store::new(db);
        let url = url::Url::parse(&format!("{base}/")).unwrap();
        let session = Arc::new(Session::new(
            Uuid::new_v4(),
            url.clone(),
            VaultHandle(Arc::new(MemoryVault::default())),
            Arc::new(|_| {}),
        ));
        session
            .store(&serde_json::from_value(json!({ "access_token": "fixture-access", "refresh_token": "fixture-refresh", "token_type": "Bearer", "expires_in": 3600,
                "user": { "id": scope.user_id, "username": "fixture", "role": "user", "created_at": "2026-10-13T12:00:00Z" } })).unwrap())
            .await;
        let http = http::Client::new(
            ApiClient::new(crate::api::http_client().unwrap(), url, session),
            CancellationToken::new(),
        );
        let data = root.path().join("data");
        let sync = Sync::new(
            http,
            Blobs::new(true).unwrap(),
            Transactions::new(store.clone(), &data),
            store.clone(),
            &data,
        );
        Self {
            root,
            scope,
            store,
            sync,
        }
    }

    fn game(&self) -> std::path::PathBuf {
        self.root.path().join("docs/game")
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::create_dir_all(self.game()).unwrap();
        std::fs::write(self.game().join(name), bytes).unwrap();
    }

    fn read(&self, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.game().join(name)).ok()
    }

    async fn pull(&self) -> Result<Pull, Error> {
        let config = SaveLocation {
            id: "game".into(),
            base: SaveBase::Documents,
            path: "game".into(),
            include: vec![],
            exclude: vec![],
        };
        let locations = Arc::new(vec![
            Location::open(&config, &self.root.path().join("docs")).unwrap(),
        ]);
        self.sync
            .pull(
                self.scope,
                locations,
                Arc::new(ProtectedInstalls::open(std::iter::empty()).unwrap()),
                CancellationToken::new(),
            )
            .await
    }

    /// Makes `files` the last synced state, as a finished restore would.
    async fn synced(&self, snapshot: Uuid, files: &[(&str, &[u8])]) {
        let id = Uuid::now_v7();
        self.store
            .begin_restore(self.scope, id, BackupReason::Restore, 0, 0)
            .await
            .unwrap();
        self.store
            .finish_restore(
                self.scope,
                id,
                Some(RestoredBase {
                    snapshot_id: Some(snapshot),
                    files: files
                        .iter()
                        .map(|(name, bytes)| (format!("game/{name}"), file(name, bytes)))
                        .collect(),
                }),
            )
            .await
            .unwrap();
    }
}

fn file(name: &str, bytes: &[u8]) -> SaveFile {
    SaveFile {
        root: "game".into(),
        path: name.into(),
        size: i64::try_from(bytes.len()).unwrap(),
        blake3: blake3::hash(bytes).to_hex().to_string(),
        mtime: time::OffsetDateTime::UNIX_EPOCH,
    }
}

fn snapshot(id: Uuid, package: Uuid, files: &[(&str, &[u8])]) -> serde_json::Value {
    let files: Vec<_> = files
        .iter()
        .map(|(name, bytes)| {
            json!({ "root": "game", "path": name, "size": bytes.len(),
                "blake3": blake3::hash(bytes).to_hex().as_str(), "mtime": "2026-10-13T12:00:00Z" })
        })
        .collect();
    let total: usize = files
        .iter()
        .map(|f| f["size"].as_u64().unwrap() as usize)
        .sum();
    json!({ "id": id, "package_id": package, "platform": "windows", "file_count": files.len(),
        "total_size": total, "created_at": "2026-10-13T12:00:00Z", "files": files })
}

async fn serve_head(server: &MockServer, rig: &Rig, head: Option<serde_json::Value>) {
    let route = format!("/v1/saves/{}/head", rig.scope.package.package_id);
    let response = match head {
        Some(body) => ResponseTemplate::new(200).set_body_json(body),
        None => ResponseTemplate::new(404).set_body_json(json!({"code": "no_saves"})),
    };
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(response)
        .mount(server)
        .await;
}

/// Serves every blob of `files`; `tamper` replaces their bytes.
async fn serve_blobs(server: &MockServer, rig: &Rig, files: &[(&str, &[u8])], tamper: bool) {
    let items: Vec<_> = files
        .iter()
        .map(|(_, bytes)| {
            let hash = blake3::hash(bytes).to_hex().to_string();
            json!({ "blake3": hash, "url": format!("{}/storage/{hash}", server.uri()),
                "expires_at": "2099-01-01T00:00:00Z" })
        })
        .collect();
    Mock::given(method("POST"))
        .and(path(format!(
            "/v1/saves/{}/blobs/download-urls",
            rig.scope.package.package_id
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": items })))
        .mount(server)
        .await;
    for (_, bytes) in files {
        let body = if tamper {
            vec![b'!'; bytes.len()]
        } else {
            bytes.to_vec()
        };
        Mock::given(method("GET"))
            .and(path(format!("/storage/{}", blake3::hash(bytes).to_hex())))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(server)
            .await;
    }
}

#[tokio::test]
async fn a_new_cloud_head_is_restored_over_unchanged_local_saves() {
    let server = MockServer::start().await;
    let rig = Rig::new(&server.uri()).await;
    let (old, head) = (Uuid::now_v7(), Uuid::now_v7());
    rig.write("old.sav", b"previous");
    rig.write("slot.sav", b"device one");
    rig.synced(
        old,
        &[("old.sav", b"previous"), ("slot.sav", b"device one")],
    )
    .await;
    let cloud: &[(&str, &[u8])] = &[("slot.sav", b"device two"), ("dlc.sav", b"new")];
    serve_head(
        &server,
        &rig,
        Some(snapshot(head, rig.scope.package.package_id, cloud)),
    )
    .await;
    serve_blobs(&server, &rig, cloud, false).await;
    assert!(matches!(rig.pull().await.unwrap(), Pull::Restored { snapshot } if snapshot == head));
    assert_eq!(rig.read("slot.sav").unwrap(), b"device two");
    assert_eq!(rig.read("dlc.sav").unwrap(), b"new");
    // Not in the head: removed, after being backed up.
    assert_eq!(rig.read("old.sav"), None);
    let state = rig.store.state(rig.scope).await.unwrap();
    assert_eq!(state.base_snapshot_id, Some(head));
    assert_eq!(state.files.len(), 2);
    assert_eq!(rig.store.backups(rig.scope).await.unwrap().len(), 2);
    // Pulling again finds nothing to do.
    assert!(matches!(rig.pull().await.unwrap(), Pull::Unchanged));
}

#[tokio::test]
async fn local_progress_conflicts_and_a_lost_head_never_touch_local_saves() {
    let server = MockServer::start().await;
    let rig = Rig::new(&server.uri()).await;
    let base = Uuid::now_v7();
    rig.write("slot.sav", b"synced");
    rig.synced(base, &[("slot.sav", b"synced")]).await;
    rig.write("slot.sav", b"played on");

    // Head unchanged: this device's progress is pushed later.
    serve_head(
        &server,
        &rig,
        Some(snapshot(
            base,
            rig.scope.package.package_id,
            &[("slot.sav", b"synced")],
        )),
    )
    .await;
    assert!(matches!(rig.pull().await.unwrap(), Pull::LocalProgress));

    // Both changed: reported, never resolved.
    server.reset().await;
    let other = Uuid::now_v7();
    serve_head(
        &server,
        &rig,
        Some(snapshot(
            other,
            rig.scope.package.package_id,
            &[("slot.sav", b"elsewhere")],
        )),
    )
    .await;
    assert!(
        matches!(rig.pull().await.unwrap(), Pull::Conflict { head } if head.summary.id == other)
    );

    // The server lost its head: local saves stay and are pushed later.
    server.reset().await;
    serve_head(&server, &rig, None).await;
    assert!(matches!(rig.pull().await.unwrap(), Pull::LocalProgress));
    assert_eq!(rig.read("slot.sav").unwrap(), b"played on");
    assert!(rig.store.backups(rig.scope).await.unwrap().len() == 1);
}

#[tokio::test]
async fn a_tampered_blob_restores_nothing() {
    let server = MockServer::start().await;
    let rig = Rig::new(&server.uri()).await;
    let cloud: &[(&str, &[u8])] = &[("slot.sav", b"cloud save")];
    serve_head(
        &server,
        &rig,
        Some(snapshot(
            Uuid::now_v7(),
            rig.scope.package.package_id,
            cloud,
        )),
    )
    .await;
    serve_blobs(&server, &rig, cloud, true).await;
    assert!(matches!(
        rig.pull().await,
        Err(Error::Transfer(blobs::Error::Mismatch))
    ));
    assert_eq!(rig.read("slot.sav"), None);
    let state = rig.store.state(rig.scope).await.unwrap();
    assert_eq!(state.base_snapshot_id, None);
    assert!(rig.store.unfinished_restores().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_unreachable_server_launches_with_local_saves_and_marks_sync_pending() {
    // wiremock pools its servers, so a dropped one may still answer: use a
    // loopback port nothing listens on (connection refused at once).
    let rig = Rig::new("http://127.0.0.1:1").await;
    rig.write("slot.sav", b"local");
    assert!(matches!(rig.pull().await.unwrap(), Pull::Offline));
    assert_eq!(rig.read("slot.sav").unwrap(), b"local");
    assert!(rig.store.state(rig.scope).await.unwrap().pending);
}

#[tokio::test]
async fn first_sync_on_an_empty_device_restores_and_an_empty_cloud_is_unchanged() {
    let server = MockServer::start().await;
    let rig = Rig::new(&server.uri()).await;
    serve_head(&server, &rig, None).await;
    assert!(matches!(rig.pull().await.unwrap(), Pull::Unchanged));

    server.reset().await;
    let head = Uuid::now_v7();
    let cloud: &[(&str, &[u8])] = &[("slot.sav", b"from the cloud")];
    serve_head(
        &server,
        &rig,
        Some(snapshot(head, rig.scope.package.package_id, cloud)),
    )
    .await;
    serve_blobs(&server, &rig, cloud, false).await;
    assert!(matches!(rig.pull().await.unwrap(), Pull::Restored { .. }));
    assert_eq!(rig.read("slot.sav").unwrap(), b"from the cloud");
    // No staged blob outlives the restore.
    let staging = rig.root.path().join("data/save-staging");
    let leftovers = walk(&staging);
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .flat_map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                walk(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}
