use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use vgames_core::manifest::{Platform, SaveBase, SaveLocation, Saves};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::api::ApiClient;
use crate::saves::sync::tests::{Rig, serve_blobs, serve_head, snapshot};

struct Fixed {
    account: Option<(Uuid, ApiClient)>,
    asked: AtomicUsize,
}

impl Accounts for Fixed {
    fn account<'a>(
        &'a self,
        _server: Uuid,
        cancel: CancellationToken,
    ) -> HookFuture<'a, Option<Account>> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        let account = self.account.clone().map(|(user_id, api)| Account {
            user_id,
            http: http::Client::new(api, cancel),
            plain_http: true,
        });
        Box::pin(async move { account })
    }
}

fn hook(rig: &Rig, signed_in: bool) -> (SaveHook, Arc<Fixed>) {
    std::fs::create_dir_all(rig.root.path().join("docs")).unwrap();
    let accounts = Arc::new(Fixed {
        account: signed_in.then(|| (rig.scope.user_id, rig.api.clone())),
        asked: AtomicUsize::new(0),
    });
    let folders = KnownFolders {
        documents: Some(rig.root.path().join("docs")),
        ..KnownFolders::default()
    };
    // A loaded CI runner can take longer than the 5 s product budget for these local pulls; the
    // budget itself is tested with a short one below.
    let hook = SaveHook::new(
        accounts.clone(),
        rig.db.clone(),
        folders,
        rig.root.path().join("data"),
    )
    .unwrap()
    .with_pull_budget(Duration::from_secs(30));
    (hook, accounts)
}

fn context(rig: &Rig, saves: bool) -> LaunchContext {
    let root = rig.root.path().join("install");
    std::fs::create_dir_all(&root).unwrap();
    LaunchContext {
        package: rig.scope.package,
        root,
        platform: Platform::LinuxX86_64,
        layer: LaunchLayer::Native,
        prefix_dir: None,
        saves: saves.then(|| {
            Arc::new(Saves {
                locations: vec![SaveLocation {
                    id: "game".into(),
                    base: SaveBase::Documents,
                    path: "game".into(),
                    include: vec![],
                    exclude: vec![],
                }],
            })
        }),
        cancellation: CancellationToken::new(),
    }
}

const EXIT: GameExit = GameExit {
    code: Some(0),
    stopped_by_user: false,
    session_seconds: 60,
};

#[tokio::test]
async fn a_newer_cloud_head_is_in_place_before_the_game_starts() {
    let server = MockServer::start().await;
    let rig = Rig::new(&server.uri()).await;
    let cloud: &[(&str, &[u8])] = &[("slot.sav", b"from the cloud")];
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
    serve_blobs(&server, &rig, cloud, false).await;
    let (hook, _) = hook(&rig, true);
    let env = hook.before_spawn(&context(&rig, true)).await.unwrap();
    assert_eq!(env, HookEnv::default());
    let state = rig.store.state(rig.scope).await.unwrap();
    assert_eq!(
        rig.read("slot.sav").as_deref(),
        Some(&b"from the cloud"[..]),
        "not restored before launch (pending: {})",
        state.pending
    );
}

#[tokio::test]
async fn a_conflict_protects_the_device_saves_and_vetoes_every_launch_until_resolved() {
    let server = MockServer::start().await;
    let rig = Rig::new(&server.uri()).await;
    rig.write("slot.sav", b"synced");
    rig.synced(Uuid::now_v7(), &[("slot.sav", b"synced")]).await;
    rig.write("slot.sav", b"played here");
    let head = Uuid::now_v7();
    serve_head(
        &server,
        &rig,
        Some(snapshot(
            head,
            rig.scope.package.package_id,
            &[("slot.sav", b"played elsewhere")],
        )),
    )
    .await;
    let (hook, _) = hook(&rig, true);
    let ctx = context(&rig, true);
    let Err(LaunchVeto::SaveConflict { conflict_id }) = hook.before_spawn(&ctx).await else {
        panic!("a conflict must veto the launch");
    };
    let record = rig
        .store
        .conflict(conflict_id.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.cloud.id, head);
    // Neither side was chosen: the device's file is untouched and backed up.
    assert_eq!(rig.read("slot.sav").unwrap(), b"played here");
    let backups = rig.store.backups(rig.scope).await.unwrap();
    assert!(backups.iter().any(|b| b.id == record.device_backup_id));

    // The next launch vetoes with the same conflict without asking the server.
    server.reset().await;
    let again = hook.before_spawn(&ctx).await;
    assert_eq!(again, Err(LaunchVeto::SaveConflict { conflict_id }));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn signed_out_or_saveless_launches_make_no_request() {
    let server = MockServer::start().await;
    let rig = Rig::new(&server.uri()).await;
    rig.write("slot.sav", b"local");
    let (signed_out, accounts) = hook(&rig, false);
    assert!(signed_out.before_spawn(&context(&rig, true)).await.is_ok());
    signed_out.after_exit(&context(&rig, true), &EXIT).await;
    assert_eq!(accounts.asked.load(Ordering::SeqCst), 2);
    let (hook, accounts) = hook(&rig, true);
    assert!(hook.before_spawn(&context(&rig, false)).await.is_ok());
    assert_eq!(accounts.asked.load(Ordering::SeqCst), 0);
    assert!(server.received_requests().await.unwrap().is_empty());
    assert_eq!(rig.read("slot.sav").unwrap(), b"local");
}

#[tokio::test]
async fn an_unreachable_server_launches_and_exits_with_local_saves_pending() {
    let rig = Rig::new("http://127.0.0.1:1").await;
    rig.write("slot.sav", b"local");
    let (hook, _) = hook(&rig, true);
    let ctx = context(&rig, true);
    assert!(hook.before_spawn(&ctx).await.is_ok());
    hook.after_exit(&ctx, &EXIT).await;
    assert_eq!(rig.read("slot.sav").unwrap(), b"local");
    assert!(rig.store.state(rig.scope).await.unwrap().pending);
}

#[tokio::test]
async fn a_slow_download_starts_the_game_with_local_saves_after_the_budget() {
    let server = MockServer::start().await;
    let rig = Rig::new(&server.uri()).await;
    rig.write("slot.sav", b"local");
    rig.synced(Uuid::now_v7(), &[("slot.sav", b"local")]).await;
    let cloud: &[(&str, &[u8])] = &[("slot.sav", b"newer")];
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
    let hash = blake3::hash(b"newer").to_hex().to_string();
    Mock::given(method("POST"))
        .and(path(format!(
            "/v1/saves/{}/blobs/download-urls",
            rig.scope.package.package_id
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [{
            "blake3": hash, "url": format!("{}/storage/{hash}", server.uri()),
            "expires_at": "2099-01-01T00:00:00Z" }] })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/storage/{hash}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"newer".to_vec())
                .set_delay(Duration::from_secs(20)),
        )
        .mount(&server)
        .await;
    let (hook, _) = hook(&rig, true);
    let hook = hook.with_pull_budget(Duration::from_secs(1));
    let started = std::time::Instant::now();
    assert!(hook.before_spawn(&context(&rig, true)).await.is_ok());
    assert!(started.elapsed() < Duration::from_secs(15));
    assert_eq!(rig.read("slot.sav").unwrap(), b"local");
    assert!(rig.store.state(rig.scope).await.unwrap().pending);
    assert!(rig.store.unfinished_restores().await.unwrap().is_empty());
}

#[tokio::test]
async fn progress_is_pushed_after_exit_and_a_lost_race_becomes_a_conflict() {
    let server = MockServer::start().await;
    let rig = Rig::new(&server.uri()).await;
    let base = Uuid::now_v7();
    rig.write("slot.sav", b"synced");
    rig.synced(base, &[("slot.sav", b"synced")]).await;
    rig.write("slot.sav", b"played on");
    Mock::given(method("POST"))
        .and(path(format!(
            "/v1/saves/{}/blobs/prepare",
            rig.scope.package.package_id
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "missing": [] })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/v1/saves/{}/snapshots",
            rig.scope.package.package_id
        )))
        .respond_with(
            ResponseTemplate::new(409).set_body_json(json!({"code": "save_head_conflict"})),
        )
        .expect(1)
        .mount(&server)
        .await;
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
    let (hook, _) = hook(&rig, true);
    hook.after_exit(&context(&rig, true), &EXIT).await;
    let open = rig
        .store
        .open_conflict_for(rig.scope)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(open.cloud.id, other);
    assert_eq!(rig.read("slot.sav").unwrap(), b"played on");
}

#[tokio::test]
async fn half_restored_saves_keep_the_game_closed_and_other_failures_do_not() {
    let rig = Rig::new("http://127.0.0.1:1").await;
    let (hook, _) = hook(&rig, true);
    let unrecovered =
        sync::Error::Transaction(transaction::Error::Unrecovered(scan::Error::Changed));
    assert_eq!(
        hook.failed(rig.scope, &unrecovered).await,
        Err(LaunchVeto::SavesUnavailable)
    );
    let undone = sync::Error::Transaction(transaction::Error::RolledBack(scan::Error::Changed));
    assert_eq!(hook.failed(rig.scope, &undone).await, Ok(()));
    assert!(rig.store.state(rig.scope).await.unwrap().pending);
}
