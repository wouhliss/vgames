//! Save blob transfers over signed URLs (06 §3). Downloads land in a private
//! staging folder under their BLAKE3 only after size and hash match; uploads
//! stream a private staged copy, never a save file the game may still change,
//! so a blob's content always equals the hash the server was told. URLs stay
//! in Rust and are never logged; no credentials or redirects are sent.
use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_LENGTH, HeaderMap, HeaderName, HeaderValue};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;
use vgames_proto::saves::{BlobDownloadUrl, BlobUploadTarget};

use super::scan;

const CONNECT: Duration = Duration::from_secs(10);
/// A stalled transfer (no bytes for this long) fails.
const STALL: Duration = Duration::from_secs(30);
/// Whole-transfer deadline: this much plus one second per `MIN_RATE` bytes.
const BASE_DEADLINE: Duration = Duration::from_secs(30);
const MIN_RATE: u64 = 256 * 1024;
const CHUNK: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("The cloud-save transfer location is not allowed.")]
    Location,
    #[error("The cloud-save storage answered {0}.")]
    Status(u16),
    #[error("The cloud-save transfer failed.")]
    Network,
    #[error("The cloud-save transfer took too long.")]
    Timeout,
    #[error("A downloaded save does not match its expected contents.")]
    Mismatch,
    #[error("Cloud-save transfer cancelled.")]
    Cancelled,
    #[error("Cannot write the downloaded save.")]
    Io(#[from] std::io::Error),
}

impl From<Error> for scan::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Mismatch => Self::Mismatch,
            Error::Cancelled => Self::Cancelled,
            Error::Io(error) => Self::Io(error),
            other => Self::Io(std::io::Error::other(other.to_string())),
        }
    }
}

#[derive(Clone)]
pub struct Blobs {
    http: reqwest::Client,
    /// Plain http only when the API itself is plain-http (debug, loopback).
    allow_http: bool,
}

impl Blobs {
    pub fn new(allow_http: bool) -> reqwest::Result<Self> {
        // No total timeout: large saves take longer than an API call. Stalls
        // and the size-scaled deadline below bound every transfer instead.
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT)
            .read_timeout(STALL)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("vgames-launcher/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { http, allow_http })
    }

    fn location(&self, raw: &str) -> Result<Url, Error> {
        let url = Url::parse(raw).map_err(|_| Error::Location)?;
        let scheme_ok = url.scheme() == "https" || (self.allow_http && url.scheme() == "http");
        if !scheme_ok || !url.username().is_empty() || url.password().is_some() {
            return Err(Error::Location);
        }
        Ok(url)
    }

    /// Downloads one blob into `staging/<blake3>`. Nothing named after the
    /// hash exists unless its size and BLAKE3 matched.
    pub async fn download(
        &self,
        target: &BlobDownloadUrl,
        size: u64,
        staging: &Path,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        if !scan::valid_hash(&target.blake3) {
            return Err(Error::Mismatch);
        }
        let url = self.location(&target.url)?;
        let temp = staging.join(format!(".vgames-{}.tmp", Uuid::now_v7().simple()));
        let result = bounded(size, cancel, async {
            let response = self.http.get(url).send().await.map_err(network)?;
            if response.status() != StatusCode::OK {
                return Err(Error::Status(response.status().as_u16()));
            }
            if response.content_length().is_some_and(|len| len != size) {
                return Err(Error::Mismatch);
            }
            let mut out = tokio::fs::File::create_new(&temp).await?;
            let mut hasher = blake3::Hasher::new();
            let mut received = 0u64;
            let mut body = response.bytes_stream();
            while let Some(chunk) = body.next().await {
                let chunk = chunk.map_err(network)?;
                received = received
                    .checked_add(chunk.len() as u64)
                    .ok_or(Error::Mismatch)?;
                if received > size {
                    return Err(Error::Mismatch);
                }
                hasher.update(&chunk);
                out.write_all(&chunk).await?;
            }
            if received != size || hasher.finalize().to_hex().as_str() != target.blake3 {
                return Err(Error::Mismatch);
            }
            out.sync_all().await?;
            drop(out);
            tokio::fs::rename(&temp, staging.join(&target.blake3)).await?;
            Ok(())
        })
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temp).await;
        }
        result
    }

    /// Uploads a staged copy (already hashed to `target.blake3`) with exactly
    /// the signed headers and its size as `Content-Length`.
    pub async fn upload(
        &self,
        target: &BlobUploadTarget,
        staged: &Path,
        size: u64,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        let url = self.location(&target.url)?;
        let mut headers = HeaderMap::new();
        for (name, value) in &target.headers {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| Error::Location)?;
            // The client owns framing and the destination; the signature may not.
            if [
                reqwest::header::HOST,
                CONTENT_LENGTH,
                reqwest::header::TRANSFER_ENCODING,
                reqwest::header::AUTHORIZATION,
            ]
            .contains(&name)
            {
                return Err(Error::Location);
            }
            headers.insert(
                name,
                HeaderValue::from_str(value).map_err(|_| Error::Location)?,
            );
        }
        let file = tokio::fs::File::open(staged).await?;
        if file.metadata().await?.len() != size {
            return Err(Error::Mismatch);
        }
        let body = futures_util::stream::unfold(file, |mut file| async move {
            let mut buffer = vec![0u8; CHUNK];
            match file.read(&mut buffer).await {
                Ok(0) => None,
                Ok(read) => {
                    buffer.truncate(read);
                    Some((Ok::<_, std::io::Error>(buffer), file))
                }
                Err(error) => Some((Err(error), file)),
            }
        });
        bounded(size, cancel, async {
            let response = self
                .http
                .put(url)
                .headers(headers)
                .header(CONTENT_LENGTH, size)
                .body(reqwest::Body::wrap_stream(body))
                .send()
                .await
                .map_err(network)?;
            if !response.status().is_success() {
                return Err(Error::Status(response.status().as_u16()));
            }
            Ok(())
        })
        .await
    }
}

async fn bounded<T>(
    size: u64,
    cancel: &CancellationToken,
    work: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    let deadline = BASE_DEADLINE + Duration::from_secs(size / MIN_RATE);
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(Error::Cancelled),
        result = tokio::time::timeout(deadline, work) => result.map_err(|_| Error::Timeout)?,
    }
}

/// Never surfaces reqwest's error text, which can contain the signed URL.
fn network(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Timeout
    } else {
        Error::Network
    }
}

#[cfg(test)]
mod tests;
