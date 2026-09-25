//! Manifest download (02 §7, first step): stream the signed manifest bytes,
//! capped at the announced size, and check their BLAKE3 before anything
//! parses them. Signature verification follows (`crate::install::verify_release`).

use std::time::Duration;

use reqwest::StatusCode;
use vgames_core::Digest;
use vgames_core::manifest::MAX_MANIFEST_BYTES;

use crate::http::redact_url;

/// Where the manifest is and what it must hash to (release descriptor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestLink {
    pub url: String,
    pub size: u64,
    pub blake3: Digest,
}

impl TryFrom<&vgames_proto::versions::ManifestLink> for ManifestLink {
    type Error = ManifestFetchError;
    fn try_from(link: &vgames_proto::versions::ManifestLink) -> Result<Self, Self::Error> {
        Ok(Self {
            url: link.url.clone(),
            size: u64::try_from(link.size).map_err(|_| ManifestFetchError::Size)?,
            blake3: link.blake3.parse().map_err(|_| ManifestFetchError::Hash)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestFetchError {
    #[error("the manifest is announced as {0} bytes, over the limit")]
    TooLarge(u64),
    #[error("the manifest cannot be downloaded: {0}")]
    Network(String),
    #[error("the storage server answered HTTP {0} for the manifest")]
    Status(u16),
    #[error("the manifest is not the announced size")]
    Size,
    #[error("the manifest does not match its announced hash")]
    Hash,
}

impl ManifestFetchError {
    fn is_transient(&self) -> bool {
        match self {
            Self::Network(_) => true,
            Self::Status(s) => *s == 408 || *s == 429 || *s >= 500,
            _ => false,
        }
    }
}

const ATTEMPTS: u32 = 3;

/// Downloads the manifest bytes and checks size and BLAKE3 (retrying
/// network errors and 5xx twice).
pub async fn fetch_manifest(
    client: &reqwest::Client,
    link: &ManifestLink,
    stall: Duration,
) -> Result<Vec<u8>, ManifestFetchError> {
    if link.size == 0 || link.size > MAX_MANIFEST_BYTES as u64 {
        return Err(ManifestFetchError::TooLarge(link.size));
    }
    let mut attempt = 0;
    loop {
        attempt += 1;
        match fetch_once(client, link, stall).await {
            Err(error) if error.is_transient() && attempt < ATTEMPTS => {
                tracing::debug!(%error, attempt, url = %redact_url(&link.url), "retrying the manifest download");
                tokio::time::sleep(Duration::from_millis(500) * attempt).await;
            }
            result => return result,
        }
    }
}

async fn fetch_once(
    client: &reqwest::Client,
    link: &ManifestLink,
    stall: Duration,
) -> Result<Vec<u8>, ManifestFetchError> {
    let network = |e: reqwest::Error| ManifestFetchError::Network(e.without_url().to_string());
    let mut response = tokio::time::timeout(stall, client.get(&link.url).send())
        .await
        .map_err(|_| ManifestFetchError::Network("no response in time".into()))?
        .map_err(network)?;
    if response.status() != StatusCode::OK {
        return Err(ManifestFetchError::Status(response.status().as_u16()));
    }
    if response.content_length().is_some_and(|n| n != link.size) {
        return Err(ManifestFetchError::Size);
    }
    let capacity = usize::try_from(link.size).map_err(|_| ManifestFetchError::Size)?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut hasher = blake3::Hasher::new();
    while let Some(piece) = tokio::time::timeout(stall, response.chunk())
        .await
        .map_err(|_| ManifestFetchError::Network("the download stalled".into()))?
        .map_err(network)?
    {
        if bytes.len() + piece.len() > capacity {
            return Err(ManifestFetchError::Size);
        }
        hasher.update(&piece);
        bytes.extend_from_slice(&piece);
    }
    if bytes.len() != capacity {
        return Err(ManifestFetchError::Size);
    }
    if Digest::from(hasher.finalize()) != link.blake3 {
        return Err(ManifestFetchError::Hash);
    }
    Ok(bytes)
}
