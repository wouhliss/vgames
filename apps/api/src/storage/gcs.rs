//! Google Cloud Storage backend: V4 signed URLs through the official client.
//!
//! Credentials come from Application Default Credentials (`GOOGLE_APPLICATION_CREDENTIALS`
//! or workload identity). With `STORAGE_EMULATOR_HOST` set, data operations go to the
//! emulator with anonymous credentials; the emulator does not check signatures.

use std::time::Duration;

use bytes::Bytes;
use google_cloud_auth::signer::Signer;
use google_cloud_storage::{
    builder::storage::SignedUrlBuilder,
    client::{Storage as GcsClient, StorageControl},
    model_ext::ReadRange,
};
use time::OffsetDateTime;

use super::{BucketKind, ByteRange, ByteStream, ObjectMeta, SignedRequest, StorageError};
use crate::config::{Config, StorageConfig};

pub struct GcsStore {
    data: GcsClient,
    control: StorageControl,
    signer: Option<Signer>,
    buckets: [String; 3],
    endpoint: Option<String>,
}

fn backend(e: impl std::fmt::Display) -> StorageError {
    StorageError::Backend(e.to_string())
}

fn is_not_found(e: &google_cloud_storage::Error) -> bool {
    e.http_status_code() == Some(404)
        || e.status()
            .is_some_and(|s| s.code == google_cloud_gax::error::rpc::Code::NotFound)
}

impl GcsStore {
    pub async fn new(config: &Config) -> Result<Self, StorageError> {
        let StorageConfig::Gcs {
            bucket_packages,
            bucket_saves,
            bucket_assets,
            emulator_host,
            ..
        } = &config.storage
        else {
            return Err(StorageError::Backend(
                "GCS backend without GCS configuration".into(),
            ));
        };
        let endpoint = emulator_host.as_ref().map(|h| {
            if h.starts_with("http://") || h.starts_with("https://") {
                h.clone()
            } else {
                format!("http://{h}")
            }
        });
        let (data, control, signer) = match &endpoint {
            Some(ep) => {
                let anon = google_cloud_auth::credentials::anonymous::Builder::new().build();
                let data = GcsClient::builder()
                    .with_endpoint(ep.clone())
                    .with_credentials(anon.clone())
                    .build()
                    .await
                    .map_err(backend)?;
                let control = StorageControl::builder()
                    .with_endpoint(ep.clone())
                    .with_credentials(anon)
                    .build()
                    .await
                    .map_err(backend)?;
                // Signing still needs a key; the emulator ignores signatures, so it is optional.
                let signer = google_cloud_auth::credentials::Builder::default()
                    .build_signer()
                    .ok();
                (data, control, signer)
            }
            None => {
                let data = GcsClient::builder().build().await.map_err(backend)?;
                let control = StorageControl::builder().build().await.map_err(backend)?;
                let signer = google_cloud_auth::credentials::Builder::default()
                    .build_signer()
                    .map_err(backend)?;
                (data, control, Some(signer))
            }
        };
        Ok(Self {
            data,
            control,
            signer,
            buckets: [
                bucket_packages.clone(),
                bucket_saves.clone(),
                bucket_assets.clone(),
            ],
            endpoint,
        })
    }

    fn bucket_id(&self, b: BucketKind) -> &str {
        match b {
            BucketKind::Packages => &self.buckets[0],
            BucketKind::Saves => &self.buckets[1],
            BucketKind::Assets => &self.buckets[2],
        }
    }

    fn bucket_path(&self, b: BucketKind) -> String {
        format!("projects/_/buckets/{}", self.bucket_id(b))
    }

    async fn sign(
        &self,
        bucket: BucketKind,
        name: &str,
        method: http::Method,
        ttl: Duration,
        headers: &[(String, String)],
    ) -> Result<SignedRequest, StorageError> {
        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| backend("no signing credentials"))?;
        let mut b = SignedUrlBuilder::for_object(self.bucket_path(bucket), name)
            .with_method(method.clone())
            .with_expiration(ttl);
        for (k, v) in headers {
            b = b.with_header(k.clone(), v.clone());
        }
        if let Some(ep) = &self.endpoint {
            b = b.with_endpoint(ep.clone());
        }
        let url = b.sign_with(signer).await.map_err(backend)?;
        let method = match method {
            http::Method::PUT => "PUT",
            http::Method::POST => "POST",
            _ => "GET",
        };
        Ok(SignedRequest {
            url,
            method,
            headers: headers.to_vec(),
            expires_at: OffsetDateTime::now_utc() + ttl,
        })
    }

    pub async fn sign_get(
        &self,
        bucket: BucketKind,
        name: &str,
        ttl: Duration,
    ) -> Result<SignedRequest, StorageError> {
        self.sign(bucket, name, http::Method::GET, ttl, &[]).await
    }

    pub async fn sign_put(
        &self,
        bucket: BucketKind,
        name: &str,
        ttl: Duration,
        content_type: &str,
        length: u64,
    ) -> Result<SignedRequest, StorageError> {
        let headers = vec![
            ("content-type".to_string(), content_type.to_string()),
            (
                "x-goog-content-length-range".to_string(),
                format!("{length},{length}"),
            ),
        ];
        self.sign(bucket, name, http::Method::PUT, ttl, &headers)
            .await
    }

    pub async fn sign_resumable_start(
        &self,
        bucket: BucketKind,
        name: &str,
        ttl: Duration,
        content_type: &str,
        min: u64,
        max: u64,
    ) -> Result<SignedRequest, StorageError> {
        let headers = vec![
            ("content-type".to_string(), content_type.to_string()),
            ("x-goog-resumable".to_string(), "start".to_string()),
            (
                "x-goog-content-length-range".to_string(),
                format!("{min},{max}"),
            ),
        ];
        self.sign(bucket, name, http::Method::POST, ttl, &headers)
            .await
    }

    pub async fn head(
        &self,
        bucket: BucketKind,
        name: &str,
    ) -> Result<Option<ObjectMeta>, StorageError> {
        match self
            .control
            .get_object()
            .set_bucket(self.bucket_path(bucket))
            .set_object(name)
            .send()
            .await
        {
            Ok(o) => Ok(Some(ObjectMeta {
                size: u64::try_from(o.size).unwrap_or_default(),
                crc32c: o.checksums.and_then(|c| c.crc32c),
            })),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(backend(e)),
        }
    }

    pub async fn get_range_stream(
        &self,
        bucket: BucketKind,
        name: &str,
        range: Option<ByteRange>,
    ) -> Result<ByteStream, StorageError> {
        let mut req = self.data.read_object(self.bucket_path(bucket), name);
        if let Some(r) = range {
            req = req.set_read_range(ReadRange::segment(
                r.start,
                r.end.saturating_sub(r.start) + 1,
            ));
        }
        let mut resp = match req.send().await {
            Ok(r) => r,
            Err(e) if is_not_found(&e) => return Err(StorageError::NotFound),
            Err(e) => return Err(backend(e)),
        };
        let stream = async_stream(move || async move {
            let next = resp.next().await;
            (next, resp)
        });
        Ok(stream)
    }

    pub async fn put_small(
        &self,
        bucket: BucketKind,
        name: &str,
        data: Bytes,
        content_type: &str,
    ) -> Result<(), StorageError> {
        Box::pin(
            self.data
                .write_object(self.bucket_path(bucket), name, data)
                .set_content_type(content_type)
                .send_buffered(),
        )
        .await
        .map_err(backend)?;
        Ok(())
    }

    pub async fn delete(&self, bucket: BucketKind, name: &str) -> Result<(), StorageError> {
        match self
            .control
            .delete_object()
            .set_bucket(self.bucket_path(bucket))
            .set_object(name)
            .send()
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(backend(e)),
        }
    }

    pub async fn list_prefix(
        &self,
        bucket: BucketKind,
        prefix: &str,
    ) -> Result<Vec<String>, StorageError> {
        let mut out = Vec::new();
        let mut token = String::new();
        loop {
            let page = self
                .control
                .list_objects()
                .set_parent(self.bucket_path(bucket))
                .set_prefix(prefix)
                .set_page_token(token.clone())
                .send()
                .await
                .map_err(backend)?;
            out.extend(page.objects.into_iter().map(|o| o.name));
            if page.next_page_token.is_empty() {
                break;
            }
            token = page.next_page_token;
        }
        out.sort();
        Ok(out)
    }
}

/// Turns a "fetch next chunk" state machine into a byte stream.
fn async_stream<F, Fut>(first: F) -> ByteStream
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<
            Output = (
                Option<google_cloud_storage::Result<Bytes>>,
                google_cloud_storage::read_object::ReadObjectResponse,
            ),
        > + Send
        + 'static,
{
    use futures_util::StreamExt;
    let init: Option<F> = Some(first);
    let stream = futures_util::stream::unfold(
        (
            init,
            None::<google_cloud_storage::read_object::ReadObjectResponse>,
        ),
        |(init, resp)| async move {
            let (next, resp) = match (init, resp) {
                (Some(f), _) => f().await,
                (None, Some(mut r)) => {
                    let n = r.next().await;
                    (n, r)
                }
                (None, None) => return None,
            };
            next.map(|item| (item.map_err(backend), (None::<F>, Some(resp))))
        },
    );
    stream.boxed()
}
