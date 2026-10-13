use super::*;
use std::time::Duration;
use vgames_proto::saves::PutMethod;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_bytes, header, method, path},
};

fn client() -> Transfers {
    crate::tls::initialize();
    Transfers(
        vgames_transfer::http::transfer_client(&vgames_transfer::http::ClientOptions {
            use_system_proxy: false,
            ..Default::default()
        })
        .unwrap(),
    )
}
fn reference(bytes: &[u8]) -> BlobRef {
    BlobRef {
        blake3: blake3::hash(bytes).to_hex().to_string(),
        size: bytes.len() as i64,
    }
}
fn download(server: &MockServer, reference: &BlobRef) -> BlobDownloadUrl {
    BlobDownloadUrl {
        blake3: reference.blake3.clone(),
        url: format!("{}/blob?fixture=opaque", server.uri()),
        expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(5),
    }
}
fn upload(server: &MockServer, reference: &BlobRef) -> BlobUploadTarget {
    BlobUploadTarget {
        blake3: reference.blake3.clone(),
        url: format!("{}/blob", server.uri()),
        method: PutMethod::Put,
        headers: std::collections::BTreeMap::from([(
            "content-type".into(),
            "application/octet-stream".into(),
        )]),
        expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(5),
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[tokio::test]
async fn verified_streamed_download_can_be_copied_and_uploaded_without_credentials() {
    let server = MockServer::start().await;
    let bytes = vec![42; BUFFER * 3 + 7];
    let reference = reference(&bytes);
    Mock::given(method("GET"))
        .and(path("/blob"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/blob"))
        .and(body_bytes(bytes.clone()))
        .and(header("content-length", reference.size.to_string()))
        .respond_with(ResponseTemplate::new(204))
        .expect(2)
        .mount(&server)
        .await;
    let staging = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let client = client();
    let blob = client
        .download(
            &download(&server, &reference),
            reference.clone(),
            staging.path(),
            deadline(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(blob.reference(), &reference);
    let output = tempfile::tempfile().unwrap();
    let mut output = File::from_std(output);
    blob.copy_to(&mut output, deadline(), &cancel)
        .await
        .unwrap();
    output.seek(std::io::SeekFrom::Start(0)).await.unwrap();
    let mut copied = Vec::new();
    output.read_to_end(&mut copied).await.unwrap();
    assert_eq!(copied, bytes);
    let target = upload(&server, &reference);
    let (first, second) = tokio::join!(
        client.upload(&target, &blob, deadline(), &cancel),
        client.upload(&target, &blob, deadline(), &cancel)
    );
    first.unwrap();
    second.unwrap();
    for request in server.received_requests().await.unwrap() {
        assert!(!request.headers.contains_key("authorization"));
        assert!(!request.headers.contains_key("cookie"));
    }
    drop(blob);
    assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn wrong_hash_size_and_redirects_never_produce_restorable_bytes() {
    let server = MockServer::start().await;
    let outside = MockServer::start().await;
    let staging = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let client = client();
    let reference = reference(b"save");
    for body in [b"evil".as_slice(), b"short".as_slice()] {
        server.reset().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;
        assert!(matches!(
            client
                .download(
                    &download(&server, &reference),
                    reference.clone(),
                    staging.path(),
                    deadline(),
                    &cancel
                )
                .await,
            Err(Error::Integrity)
        ));
        assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
    }
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("Location", format!("{}/blob", outside.uri())),
        )
        .mount(&server)
        .await;
    assert!(matches!(
        client
            .download(
                &download(&server, &reference),
                reference,
                staging.path(),
                deadline(),
                &cancel
            )
            .await,
        Err(Error::Rejected)
    ));
    assert!(outside.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_and_total_deadline_abort_download_before_staging() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(3))
                .set_body_bytes(b"save"),
        )
        .mount(&server)
        .await;
    let staging = tempfile::tempdir().unwrap();
    let reference = reference(b"save");
    let target = download(&server, &reference);
    let client = client();
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        stop.cancel();
    });
    assert!(matches!(
        client
            .download(
                &target,
                reference.clone(),
                staging.path(),
                deadline(),
                &cancel
            )
            .await,
        Err(Error::Cancelled)
    ));
    task.await.unwrap();
    assert!(matches!(
        client
            .download(
                &target,
                reference,
                staging.path(),
                Instant::now() + Duration::from_millis(30),
                &CancellationToken::new()
            )
            .await,
        Err(Error::Timeout)
    ));
    assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn local_staging_checks_integrity_and_freezes_bytes_before_upload() {
    let source = tempfile::tempdir().unwrap();
    let staging = tempfile::tempdir().unwrap();
    let path = source.path().join("slot.sav");
    std::fs::write(&path, b"save").unwrap();
    let reference = reference(b"save");
    let cancel = CancellationToken::new();
    let blob = stage(
        std::fs::File::open(&path).unwrap(),
        reference.clone(),
        staging.path(),
        deadline(),
        &cancel,
    )
    .await
    .unwrap();
    std::fs::write(&path, b"next").unwrap();
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(body_bytes(b"save"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    client()
        .upload(&upload(&server, &reference), &blob, deadline(), &cancel)
        .await
        .unwrap();
    assert!(matches!(
        stage(
            std::fs::File::open(&path).unwrap(),
            reference,
            staging.path(),
            deadline(),
            &cancel
        )
        .await,
        Err(Error::Integrity)
    ));
}

#[tokio::test]
async fn invalid_urls_expiry_metadata_and_credential_headers_are_refused_without_requests() {
    for value in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "https://user:pass@storage.example/blob",
        "https://storage.example/blob#fragment",
        "http://storage.example/blob",
        " https://storage.example/blob",
    ] {
        assert!(matches!(url(value), Err(Error::Invalid)));
    }
    let server = MockServer::start().await;
    let staging = tempfile::tempdir().unwrap();
    let reference = reference(b"");
    let cancel = CancellationToken::new();
    let blob = stage(
        tempfile::tempfile().unwrap(),
        reference.clone(),
        staging.path(),
        deadline(),
        &cancel,
    )
    .await
    .unwrap();
    let client = client();
    for name in [
        "authorization",
        "cookie",
        "host",
        "content-length",
        "transfer-encoding",
    ] {
        let mut target = upload(&server, &reference);
        target.headers.insert(name.into(), "fixture".into());
        assert!(matches!(
            client.upload(&target, &blob, deadline(), &cancel).await,
            Err(Error::Invalid)
        ));
    }
    let mut target = download(&server, &reference);
    assert!(matches!(
        client
            .download(
                &target,
                reference.clone(),
                staging.path(),
                Instant::now() - Duration::from_secs(1),
                &cancel
            )
            .await,
        Err(Error::Timeout)
    ));
    target.expires_at = time::OffsetDateTime::UNIX_EPOCH;
    assert!(matches!(
        client
            .download(
                &target,
                reference.clone(),
                staging.path(),
                deadline(),
                &cancel
            )
            .await,
        Err(Error::Expired)
    ));
    let invalid = BlobRef {
        size: MAX_BYTES + 1,
        ..reference
    };
    assert!(matches!(validate(&invalid), Err(Error::Invalid)));
    assert!(server.received_requests().await.unwrap().is_empty());
}
