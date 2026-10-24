use std::collections::BTreeMap;

use time::OffsetDateTime;
use vgames_proto::saves::PutMethod;
use wiremock::matchers::{body_bytes, header, headers, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn download_target(server: &MockServer, bytes: &[u8]) -> BlobDownloadUrl {
    BlobDownloadUrl {
        blake3: hash(bytes),
        url: format!("{}/blob?X-Goog-Signature=secret", server.uri()),
        expires_at: OffsetDateTime::now_utc(),
    }
}

fn staged_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_download_is_kept_only_when_size_and_hash_match() {
    let server = MockServer::start().await;
    let staging = tempfile::tempdir().unwrap();
    let blobs = Blobs::new(true).unwrap();
    let cancel = CancellationToken::new();
    let good = b"save data";
    Mock::given(method("GET"))
        .and(path("/blob"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(good.to_vec()))
        .mount(&server)
        .await;
    blobs
        .download(&download_target(&server, good), 9, staging.path(), &cancel)
        .await
        .unwrap();
    assert_eq!(staged_names(staging.path()), [hash(good)]);
    assert_eq!(
        std::fs::read(staging.path().join(hash(good))).unwrap(),
        good
    );

    // The same bytes under another expected hash, or with another size,
    // leave nothing behind (no temporary file either).
    for (target, size) in [
        (download_target(&server, b"other"), 9),
        (download_target(&server, good), 8),
        (download_target(&server, good), 10),
    ] {
        let fresh = tempfile::tempdir().unwrap();
        assert!(matches!(
            blobs.download(&target, size, fresh.path(), &cancel).await,
            Err(Error::Mismatch)
        ));
        assert!(staged_names(fresh.path()).is_empty());
    }
}

#[tokio::test]
async fn failed_and_refused_downloads_name_no_url() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let staging = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let target = download_target(&server, b"x");
    let error = Blobs::new(true)
        .unwrap()
        .download(&target, 1, staging.path(), &cancel)
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Status(403)));
    // Plain http is refused unless the API itself is plain http.
    let refused = Blobs::new(false)
        .unwrap()
        .download(&target, 1, staging.path(), &cancel)
        .await
        .unwrap_err();
    assert!(matches!(refused, Error::Location));
    for message in [error.to_string(), refused.to_string()] {
        assert!(!message.contains("Signature"), "{message}");
        assert!(!message.contains("127.0.0.1"), "{message}");
    }
    let mut credentials = target.clone();
    credentials.url = "https://user:pass@storage.example.invalid/blob".into();
    assert!(matches!(
        Blobs::new(false)
            .unwrap()
            .download(&credentials, 1, staging.path(), &cancel)
            .await,
        Err(Error::Location)
    ));
    assert!(staged_names(staging.path()).is_empty());
}

#[tokio::test]
async fn uploads_send_the_staged_copy_with_the_signed_headers() {
    let server = MockServer::start().await;
    let bytes = vec![7u8; 200_000];
    Mock::given(method("PUT"))
        .and(path("/upload"))
        // `header` splits values on commas; the signed value is one list.
        .and(headers(
            "x-goog-content-length-range",
            vec!["200000", "200000"],
        ))
        .and(header("content-length", "200000"))
        .and(body_bytes(bytes.clone()))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let staged = dir.path().join("copy");
    std::fs::write(&staged, &bytes).unwrap();
    let target = BlobUploadTarget {
        blake3: hash(&bytes),
        url: format!("{}/upload", server.uri()),
        method: PutMethod::Put,
        headers: BTreeMap::from([(
            "x-goog-content-length-range".to_owned(),
            "200000,200000".to_owned(),
        )]),
        expires_at: OffsetDateTime::now_utc(),
    };
    let blobs = Blobs::new(true).unwrap();
    let cancel = CancellationToken::new();
    blobs
        .upload(&target, &staged, 200_000, &cancel)
        .await
        .unwrap();

    // A staged copy of another size is never sent.
    assert!(matches!(
        blobs.upload(&target, &staged, 199_999, &cancel).await,
        Err(Error::Mismatch)
    ));
    // Signed headers may not take over framing, destination or credentials.
    for name in [
        "content-length",
        "host",
        "authorization",
        "transfer-encoding",
    ] {
        let mut hostile = target.clone();
        hostile.headers.insert(name.into(), "1".into());
        assert!(matches!(
            blobs.upload(&hostile, &staged, 200_000, &cancel).await,
            Err(Error::Location)
        ));
    }
}

#[tokio::test]
async fn cancellation_stops_a_transfer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"late".to_vec())
                .set_delay(Duration::from_secs(30)),
        )
        .mount(&server)
        .await;
    let staging = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        stop.cancel();
    });
    let started = std::time::Instant::now();
    assert!(matches!(
        Blobs::new(true)
            .unwrap()
            .download(
                &download_target(&server, b"late"),
                4,
                staging.path(),
                &cancel
            )
            .await,
        Err(Error::Cancelled)
    ));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(staged_names(staging.path()).is_empty());
}
