//! Verified, bounded staging and streaming transfers. Temporary bytes never
//! touch game files; only a complete hash-checked blob can be restored/uploaded.
use std::{future::Future, path::Path, sync::Arc};

use reqwest::{
    StatusCode, Url,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::Mutex,
    time::Instant,
};
use tokio_util::sync::CancellationToken;
use vgames_proto::saves::{BlobDownloadUrl, BlobRef, BlobUploadTarget};

const MAX_BYTES: i64 = 512 * 1024 * 1024;
const BUFFER: usize = 64 * 1024;

// Underlying HTTP errors may contain signed URLs; never retain them in chains.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid save transfer metadata.")]
    Invalid,
    #[error("Save blob size or hash verification failed.")]
    Integrity,
    #[error("Save storage is unavailable.")]
    Storage,
    #[error("Save transfer failed.")]
    Network,
    #[error("Save transfer was rejected.")]
    Rejected,
    #[error("Save transfer URL expired.")]
    Expired,
    #[error("Save transfer cancelled.")]
    Cancelled,
    #[error("Save transfer timed out.")]
    Timeout,
}

/// Private anonymous staging file. The lock serializes shared file offsets;
/// callers cannot mutate bytes after verification or obtain an absolute path.
pub struct StagedBlob {
    reference: BlobRef,
    file: Arc<Mutex<File>>,
}
impl StagedBlob {
    pub fn reference(&self) -> &BlobRef {
        &self.reference
    }
    /// Copy only into a restore temporary file, never a game's live save file.
    /// The restore transaction flushes and atomically replaces it separately.
    pub async fn copy_to(
        &self,
        output: &mut File,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        bounded(deadline, cancel, async {
            let mut file = self.file.lock().await;
            file.seek(std::io::SeekFrom::Start(0))
                .await
                .map_err(|_| Error::Storage)?;
            let copied = tokio::io::copy(&mut *file, output)
                .await
                .map_err(|_| Error::Storage)?;
            if copied != self.reference.size as u64 {
                return Err(Error::Integrity);
            }
            Ok(())
        })
        .await
    }
}

fn validate(reference: &BlobRef) -> Result<(), Error> {
    if !(0..=MAX_BYTES).contains(&reference.size)
        || reference.blake3.len() != 64
        || !reference
            .blake3
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn url(value: &str) -> Result<Url, Error> {
    if value.trim() != value || value.bytes().any(|b| b.is_ascii_control()) {
        return Err(Error::Invalid);
    }
    let url = Url::parse(value).map_err(|_| Error::Invalid)?;
    let loopback = cfg!(debug_assertions)
        && url.scheme() == "http"
        && url.host_str().is_some_and(|host| {
            host == "localhost"
                || host == "[::1]"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (url.scheme() != "https" && !loopback)
    {
        return Err(Error::Invalid);
    }
    Ok(url)
}
async fn bounded<T>(
    deadline: Instant,
    cancel: &CancellationToken,
    work: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    if Instant::now() >= deadline {
        return Err(Error::Timeout);
    }
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(Error::Cancelled),
        result = tokio::time::timeout_at(deadline, work) => result.map_err(|_| Error::Timeout)?,
    }
}
async fn temporary(directory: &Path) -> Result<File, Error> {
    let directory = directory.to_owned();
    // Directory comes only from native app paths. No created name or URL is
    // exposed to the WebView; closing the anonymous handle removes the bytes.
    let file = tokio::task::spawn_blocking(move || tempfile::tempfile_in(directory))
        .await
        .map_err(|_| Error::Storage)?
        .map_err(|_| Error::Storage)?;
    Ok(File::from_std(file))
}
fn complete(hash: blake3::Hasher, count: u64, reference: &BlobRef) -> Result<(), Error> {
    if count != reference.size as u64 || hash.finalize().to_hex().as_str() != reference.blake3 {
        return Err(Error::Integrity);
    }
    Ok(())
}
async fn seal(mut file: File, reference: BlobRef) -> Result<StagedBlob, Error> {
    file.flush().await.map_err(|_| Error::Storage)?;
    file.sync_all().await.map_err(|_| Error::Storage)?;
    Ok(StagedBlob {
        reference,
        file: Arc::new(Mutex::new(file)),
    })
}

/// `source` must be an already opened no-follow save handle. Copy and verify
/// before any network request, so a changing game file cannot change an upload.
pub async fn stage(
    source: std::fs::File,
    reference: BlobRef,
    directory: &Path,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<StagedBlob, Error> {
    validate(&reference)?;
    bounded(deadline, cancel, async {
        let mut source = File::from_std(source);
        source
            .seek(std::io::SeekFrom::Start(0))
            .await
            .map_err(|_| Error::Storage)?;
        let mut output = temporary(directory).await?;
        let mut buffer = vec![0; BUFFER];
        let mut count = 0;
        let mut hash = blake3::Hasher::new();
        loop {
            let read = source.read(&mut buffer).await.map_err(|_| Error::Storage)?;
            if read == 0 {
                break;
            }
            count += read as u64;
            if count > reference.size as u64 {
                return Err(Error::Integrity);
            }
            let bytes = buffer.get(..read).ok_or(Error::Storage)?;
            hash.update(bytes);
            output.write_all(bytes).await.map_err(|_| Error::Storage)?;
        }
        complete(hash, count, &reference)?;
        seal(output, reference).await
    })
    .await
}

pub struct Transfers(reqwest::Client);
impl Transfers {
    pub fn new() -> Result<Self, Error> {
        // No auth/cookie defaults, no redirects and no transparent decompression.
        vgames_transfer::http::transfer_client(&Default::default())
            .map(Self)
            .map_err(|_| Error::Network)
    }
    pub async fn download(
        &self,
        target: &BlobDownloadUrl,
        reference: BlobRef,
        directory: &Path,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<StagedBlob, Error> {
        validate(&reference)?;
        if target.blake3 != reference.blake3 {
            return Err(Error::Invalid);
        }
        if target.expires_at <= time::OffsetDateTime::now_utc() {
            return Err(Error::Expired);
        }
        let url = url(&target.url)?;
        bounded(deadline, cancel, async {
            let mut response = self.0.get(url).send().await.map_err(|_| Error::Network)?;
            if response.status() != StatusCode::OK {
                return Err(Error::Rejected);
            }
            if response
                .content_length()
                .is_some_and(|size| size != reference.size as u64)
            {
                return Err(Error::Integrity);
            }
            let mut output = temporary(directory).await?;
            let mut count = 0;
            let mut hash = blake3::Hasher::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| Error::Network)? {
                count += chunk.len() as u64;
                if count > reference.size as u64 {
                    return Err(Error::Integrity);
                }
                hash.update(&chunk);
                output.write_all(&chunk).await.map_err(|_| Error::Storage)?;
            }
            complete(hash, count, &reference)?;
            seal(output, reference).await
        })
        .await
    }
    pub async fn upload(
        &self,
        target: &BlobUploadTarget,
        blob: &StagedBlob,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        if target.blake3 != blob.reference.blake3 {
            return Err(Error::Invalid);
        }
        if target.expires_at <= time::OffsetDateTime::now_utc() {
            return Err(Error::Expired);
        }
        let url = url(&target.url)?;
        let mut headers = HeaderMap::new();
        for (name, value) in &target.headers {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| Error::Invalid)?;
            // Only object-storage signature headers; never forward credentials,
            // host overrides or framing supplied by a compromised server.
            if !matches!(name.as_str(), "content-type" | "content-md5")
                && !name.as_str().starts_with("x-goog-")
            {
                return Err(Error::Invalid);
            }
            let mut value = HeaderValue::from_str(value).map_err(|_| Error::Invalid)?;
            value.set_sensitive(true);
            if headers.insert(name, value).is_some() {
                return Err(Error::Invalid);
            }
        }
        bounded(deadline, cancel, async {
            let mut file = blob.file.clone().lock_owned().await;
            file.seek(std::io::SeekFrom::Start(0))
                .await
                .map_err(|_| Error::Storage)?;
            let stream = futures_util::stream::try_unfold(
                (file, blob.reference.size as u64),
                |(mut file, remaining)| async move {
                    if remaining == 0 {
                        return Ok::<_, std::io::Error>(None);
                    }
                    let mut buffer = vec![0; BUFFER.min(remaining as usize)];
                    let read = file.read(&mut buffer).await?;
                    if read == 0 {
                        return Err(std::io::ErrorKind::UnexpectedEof.into());
                    }
                    buffer.truncate(read);
                    Ok(Some((buffer, (file, remaining - read as u64))))
                },
            );
            let response = self
                .0
                .put(url)
                .headers(headers)
                .header("Content-Length", blob.reference.size)
                .body(reqwest::Body::wrap_stream(stream))
                .send()
                .await
                .map_err(|_| Error::Network)?;
            if !matches!(
                response.status(),
                StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT
            ) {
                return Err(Error::Rejected);
            }
            // Snapshot commit confirms object metadata/size on the real server.
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests;
