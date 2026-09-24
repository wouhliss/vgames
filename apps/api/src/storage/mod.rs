//! Object storage (A1-T07): Google Cloud Storage in production, a local `fs` backend for
//! development, tests and self-hosters without GCS.
//!
//! Bulk bytes never pass through API handlers: clients get short-lived signed URLs. The
//! `fs` backend serves those URLs itself under `/_storage/*` and speaks the same wire
//! protocols clients use against GCS (Range GETs, single PUTs, resumable sessions), so
//! launcher and uploader code cannot tell the backends apart.

pub mod fs;
pub mod gcs;

use std::{pin::Pin, time::Duration};

use bytes::Bytes;
use futures_util::Stream;
use time::OffsetDateTime;

use crate::config::{Config, StorageConfig};

/// The three buckets (docs/architecture/02-package-format.md §2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BucketKind {
    Packages,
    Saves,
    Assets,
}

impl BucketKind {
    pub const ALL: [BucketKind; 3] = [BucketKind::Packages, BucketKind::Saves, BucketKind::Assets];

    pub fn as_str(self) -> &'static str {
        match self {
            BucketKind::Packages => "packages",
            BucketKind::Saves => "saves",
            BucketKind::Assets => "assets",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.as_str() == s)
    }
}

/// A request a client may make without credentials until `expires_at`. The listed headers
/// are part of the signature and must be sent exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedRequest {
    pub url: String,
    pub method: &'static str,
    pub headers: Vec<(String, String)>,
    pub expires_at: OffsetDateTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectMeta {
    pub size: u64,
    pub crc32c: Option<u32>,
}

/// Inclusive byte range `[start, end]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("invalid object name")]
    InvalidName,
    #[error("object not found")]
    NotFound,
    #[error("storage backend error: {0}")]
    Backend(String),
    #[error("storage i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<StorageError> for crate::error::ApiError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::NotFound => crate::error::ApiError::not_found(),
            other => crate::error::ApiError::internal_from(other),
        }
    }
}

pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, StorageError>> + Send>>;

/// Object names: 1–512 chars of `[A-Za-z0-9._/-]`, no empty, `.` or `..` segments, no
/// leading or trailing `/`. Anything else is refused before it reaches a backend.
pub fn validate_name(name: &str) -> Result<(), StorageError> {
    let ok = (1..=512).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
        && name
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..");
    if ok {
        Ok(())
    } else {
        Err(StorageError::InvalidName)
    }
}

/// The configured backend.
pub enum Storage {
    Fs(fs::FsStore),
    Gcs(Box<gcs::GcsStore>),
}

impl Storage {
    pub async fn from_config(config: &Config) -> Result<Self, StorageError> {
        match &config.storage {
            StorageConfig::Fs { root, signing_key } => Ok(Storage::Fs(fs::FsStore::new(
                root.clone(),
                signing_key.expose(),
                config.public_origin(),
            )?)),
            StorageConfig::Gcs { .. } => {
                Ok(Storage::Gcs(Box::new(gcs::GcsStore::new(config).await?)))
            }
        }
    }

    /// Signed GET for downloads (Range requests allowed).
    pub async fn sign_get(
        &self,
        bucket: BucketKind,
        name: &str,
        ttl: Duration,
    ) -> Result<SignedRequest, StorageError> {
        validate_name(name)?;
        match self {
            Storage::Fs(s) => s.sign_get(bucket, name, ttl),
            Storage::Gcs(s) => s.sign_get(bucket, name, ttl).await,
        }
    }

    /// Signed single-shot PUT of exactly `length` bytes of `content_type`.
    pub async fn sign_put(
        &self,
        bucket: BucketKind,
        name: &str,
        ttl: Duration,
        content_type: &str,
        length: u64,
    ) -> Result<SignedRequest, StorageError> {
        validate_name(name)?;
        match self {
            Storage::Fs(s) => s.sign_put(bucket, name, ttl, content_type, length),
            Storage::Gcs(s) => s.sign_put(bucket, name, ttl, content_type, length).await,
        }
    }

    /// Signed POST that starts a resumable upload (`x-goog-resumable: start`); the
    /// response's `Location` is the session URI. The final object must be `min..=max` bytes.
    pub async fn sign_resumable_start(
        &self,
        bucket: BucketKind,
        name: &str,
        ttl: Duration,
        content_type: &str,
        min: u64,
        max: u64,
    ) -> Result<SignedRequest, StorageError> {
        validate_name(name)?;
        match self {
            Storage::Fs(s) => s.sign_resumable_start(bucket, name, ttl, content_type, min, max),
            Storage::Gcs(s) => {
                s.sign_resumable_start(bucket, name, ttl, content_type, min, max)
                    .await
            }
        }
    }

    /// Size (and CRC32C where the backend has it), or `None` when absent.
    pub async fn head(
        &self,
        bucket: BucketKind,
        name: &str,
    ) -> Result<Option<ObjectMeta>, StorageError> {
        validate_name(name)?;
        match self {
            Storage::Fs(s) => s.head(bucket, name).await,
            Storage::Gcs(s) => s.head(bucket, name).await,
        }
    }

    /// Streams `range` (or the whole object).
    pub async fn get_range_stream(
        &self,
        bucket: BucketKind,
        name: &str,
        range: Option<ByteRange>,
    ) -> Result<ByteStream, StorageError> {
        validate_name(name)?;
        match self {
            Storage::Fs(s) => s.get_range_stream(bucket, name, range).await,
            Storage::Gcs(s) => s.get_range_stream(bucket, name, range).await,
        }
    }

    /// Server-side write of a small object (images, signature envelopes).
    pub async fn put_small(
        &self,
        bucket: BucketKind,
        name: &str,
        data: Bytes,
        content_type: &str,
    ) -> Result<(), StorageError> {
        validate_name(name)?;
        match self {
            Storage::Fs(s) => s.put_small(bucket, name, data).await,
            Storage::Gcs(s) => s.put_small(bucket, name, data, content_type).await,
        }
    }

    /// Deletes an object; deleting a missing object succeeds.
    pub async fn delete(&self, bucket: BucketKind, name: &str) -> Result<(), StorageError> {
        validate_name(name)?;
        match self {
            Storage::Fs(s) => s.delete(bucket, name).await,
            Storage::Gcs(s) => s.delete(bucket, name).await,
        }
    }

    /// Object names under `prefix` (sorted).
    pub async fn list_prefix(
        &self,
        bucket: BucketKind,
        prefix: &str,
    ) -> Result<Vec<String>, StorageError> {
        if !prefix.is_empty() {
            validate_name(prefix.trim_end_matches('/'))?;
        }
        match self {
            Storage::Fs(s) => s.list_prefix(bucket, prefix).await,
            Storage::Gcs(s) => s.list_prefix(bucket, prefix).await,
        }
    }

    /// Health probe: a `head` of a sentinel object succeeds (missing is fine).
    pub async fn ping(&self) -> bool {
        self.head(BucketKind::Assets, "_health/sentinel")
            .await
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_names() {
        for ok in [
            "v1/pkg/ver/packs/00001.pack",
            "a",
            "v1/u/0123abcd",
            "x-y_z.1",
        ] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/abs",
            "trail/",
            "a//b",
            "a/../b",
            "..",
            ".",
            "a/./b",
            "sp ace",
            "back\\slash",
            "q?x",
            &"a".repeat(513),
        ] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }
}
