use super::*;
use crate::api::Session;
use crate::secrets::{MemoryVault, VaultHandle};
use serde_json::json;
use std::sync::Arc;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn client(server: &MockServer) -> Client {
    let base = url::Url::parse(&format!("{}/", server.uri())).unwrap();
    let session = Arc::new(Session::new(
        Uuid::new_v4(),
        base.clone(),
        VaultHandle(Arc::new(MemoryVault::default())),
        Arc::new(|_| {}),
    ));
    session.store(&serde_json::from_value(json!({ "access_token": "fixture-access", "refresh_token": "fixture-refresh", "token_type": "Bearer", "expires_in": 3600,
        "user": { "id": Uuid::new_v4(), "username": "fixture", "role": "user", "created_at": "2026-10-13T12:00:00Z" } })).unwrap()).await;
    Client::new(
        ApiClient::new(crate::api::http_client().unwrap(), base, session),
        CancellationToken::new(),
    )
}
fn snapshot(package: Uuid) -> serde_json::Value {
    json!({ "id": Uuid::new_v4(), "package_id": package, "platform": "windows", "file_count": 0, "total_size": 0, "created_at": "2026-10-13T12:00:00Z", "files": [] })
}
#[tokio::test]
async fn missing_head_differs_from_unknown_package_and_wrong_package_responses_are_rejected() {
    let server = MockServer::start().await;
    let client = client(&server).await;
    let package = Uuid::new_v4();
    let route = format!("/v1/saves/{package}/head");
    Mock::given(method("GET"))
        .and(path(&route))
        .and(header("authorization", "Bearer fixture-access"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"code":"no_saves"})))
        .mount(&server)
        .await;
    assert!(client.head(package).await.unwrap().is_none());
    server.reset().await;
    Mock::given(method("GET"))
        .and(path(&route))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"code":"not_found"})))
        .mount(&server)
        .await;
    assert!(
        matches!(client.head(package).await, Err(Error::Api(error)) if error.problem_code() == Some("not_found"))
    );
    server.reset().await;
    Mock::given(method("GET"))
        .and(path(&route))
        .respond_with(ResponseTemplate::new(200).set_body_json(snapshot(Uuid::new_v4())))
        .mount(&server)
        .await;
    assert!(matches!(
        client.head(package).await,
        Err(Error::Api(ApiError::InvalidResponse(_)))
    ));
}
#[tokio::test]
async fn competing_head_error_and_same_operation_retries_keep_the_idempotency_key() {
    let server = MockServer::start().await;
    let client = client(&server).await;
    let package = Uuid::new_v4();
    let operation = Uuid::new_v4();
    let request = CommitSnapshotRequest {
        parent_snapshot_id: None,
        platform: vgames_proto::saves::OsFamily::Windows,
        label: None,
        files: Vec::new(),
    };
    let route = format!("/v1/saves/{package}/snapshots");
    Mock::given(method("POST"))
        .and(path(&route))
        .and(header("Idempotency-Key", operation.to_string()))
        .respond_with(
            ResponseTemplate::new(409).set_body_json(json!({"code":"save_head_conflict"})),
        )
        .mount(&server)
        .await;
    assert!(
        matches!(client.commit(package, &request, operation).await, Err(Error::Api(error)) if error.problem_code() == Some("save_head_conflict"))
    );
    server.reset().await;
    let response = snapshot(package);
    Mock::given(method("POST"))
        .and(path(&route))
        .and(header("Idempotency-Key", operation.to_string()))
        .respond_with(ResponseTemplate::new(201).set_body_json(&response))
        .expect(2)
        .mount(&server)
        .await;
    let first = client.commit(package, &request, operation).await.unwrap();
    let retry = client.commit(package, &request, operation).await.unwrap();
    assert_eq!(first, retry);
    for request in server.received_requests().await.unwrap() {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            body.get("parent_snapshot_id"),
            Some(&serde_json::Value::Null)
        );
    }
}
#[tokio::test]
async fn cancellation_aborts_an_in_flight_authenticated_request() {
    let server = MockServer::start().await;
    let client = client(&server).await;
    let cancel = client.cancel.clone();
    let package = Uuid::new_v4();
    Mock::given(method("GET"))
        .and(path(format!("/v1/saves/{package}/head")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(30))
                .set_body_json(snapshot(package)),
        )
        .mount(&server)
        .await;
    let request = tokio::spawn(async move { client.head(package).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !server.received_requests().await.unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), request)
            .await
            .unwrap()
            .unwrap(),
        Err(Error::Cancelled)
    ));
}

#[tokio::test]
async fn history_keeps_reverse_proxy_prefix_and_encodes_opaque_cursor_as_one_query_value() {
    let server = MockServer::start().await;
    let mut client = client(&server).await;
    let package = Uuid::new_v4();
    let base = url::Url::parse(&format!("{}/prefix/", server.uri())).unwrap();
    client.api = ApiClient::new(
        crate::api::http_client().unwrap(),
        base,
        client.api.session().clone(),
    );
    Mock::given(method("GET"))
        .and(path(format!("/prefix/v1/saves/{package}/snapshots")))
        .and(wiremock::matchers::query_param(
            "cursor",
            "opaque/=?&other=1",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "items": [], "next_cursor": null })),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert!(
        client
            .history_page(package, Some("opaque/=?&other=1"))
            .await
            .unwrap()
            .items
            .is_empty()
    );
}
