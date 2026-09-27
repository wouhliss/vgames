//! Disposable cover-art cache for the `vgimg://` protocol. All methods do
//! blocking disk I/O and must be called from a blocking pool by async callers.
//! The cache key contains no remote URL, token, or server-provided path.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use tauri::http::{self, Method, Request, Response, StatusCode, header};
use uuid::Uuid;

/// Maximum response body accepted for one image (10 MiB).
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// Maximum total size of cached images (500 MB).
pub const MAX_CACHE_BYTES: u64 = 500_000_000;

#[derive(Debug, thiserror::Error)]
pub enum ImageCacheError {
    #[error("the image is empty, too large, or does not match its content type")]
    InvalidImage,
    #[error("this content type is not a supported raster image")]
    UnsupportedType,
    #[error("the asset identifier is invalid")]
    InvalidAsset,
    #[error("the image cache directory is not a real directory")]
    InvalidDirectory,
    #[error("image cache lock was poisoned")]
    LockPoisoned,
    #[error("image cache I/O failed")]
    Io(#[from] io::Error),
}

/// A server-scoped, opaque identifier safe to put in a local cache filename.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageKey(String);

impl ImageKey {
    pub fn for_asset(server_id: Uuid, asset_id: &str) -> Result<Self, ImageCacheError> {
        if asset_id.is_empty() || asset_id.len() > 2048 {
            return Err(ImageCacheError::InvalidAsset);
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"vgames-image-cache-v1");
        hasher.update(server_id.as_bytes());
        hasher.update(&(asset_id.len() as u32).to_le_bytes());
        hasher.update(asset_id.as_bytes());
        Ok(Self(hasher.finalize().to_hex().to_string()))
    }

    fn from_cache_id(value: &str) -> Option<Self> {
        (value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then(|| Self(value.to_owned()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageType {
    Jpeg,
    Png,
    Webp,
}

impl ImageType {
    fn from_content_type(value: &str) -> Result<Self, ImageCacheError> {
        match value
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "image/jpeg" => Ok(Self::Jpeg),
            "image/png" => Ok(Self::Png),
            "image/webp" => Ok(Self::Webp),
            _ => Err(ImageCacheError::UnsupportedType),
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
        }
    }

    fn matches_bytes(self, bytes: &[u8]) -> bool {
        match self {
            Self::Jpeg => bytes.starts_with(&[0xff, 0xd8, 0xff]),
            Self::Png => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            Self::Webp => {
                bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice())
            }
        }
    }
}

const IMAGE_TYPES: [ImageType; 3] = [ImageType::Jpeg, ImageType::Png, ImageType::Webp];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedImage {
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

/// One launcher-owned cache directory. The mutex serializes writes and LRU
/// eviction; it never encloses network work.
pub struct ImageCache {
    root: PathBuf,
    budget: u64,
    lock: Mutex<()>,
}

impl ImageCache {
    pub fn open(root: PathBuf) -> Result<Self, ImageCacheError> {
        Self::with_budget(root, MAX_CACHE_BYTES)
    }

    fn with_budget(root: PathBuf, budget: u64) -> Result<Self, ImageCacheError> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&root)?;
        if !fs::symlink_metadata(&root)?.file_type().is_dir() {
            return Err(ImageCacheError::InvalidDirectory);
        }
        let cache = Self {
            root,
            budget,
            lock: Mutex::new(()),
        };
        cache.evict_locked()?;
        Ok(cache)
    }

    /// A corrupt or substituted cache entry is a miss. It is never served.
    pub fn get(&self, key: &ImageKey) -> Result<Option<CachedImage>, ImageCacheError> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| ImageCacheError::LockPoisoned)?;
        for kind in IMAGE_TYPES {
            let path = self.path(key, kind);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if !metadata.file_type().is_file() || metadata.len() > MAX_IMAGE_BYTES as u64 {
                fs::remove_file(&path)?;
                continue;
            }
            // Refreshing mtime needs a writable handle on Windows. A read-only
            // fallback still serves a valid cache entry when that is denied.
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .or_else(|_| File::open(&path))?;
            let mut bytes = Vec::new();
            (&mut file)
                .take(MAX_IMAGE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES || !kind.matches_bytes(&bytes) {
                // Windows refuses to remove a file while this handle is open.
                drop(file);
                fs::remove_file(&path)?;
                continue;
            }
            if let Err(error) = file.set_modified(SystemTime::now()) {
                tracing::debug!(%error, "cannot update image cache recency");
            }
            return Ok(Some(CachedImage {
                content_type: kind.content_type(),
                bytes,
            }));
        }
        Ok(None)
    }

    /// Stores a fully bounded body returned by the native API client. The
    /// caller must cap the HTTP stream before accumulating its bytes in memory.
    pub fn insert(
        &self,
        key: &ImageKey,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<(), ImageCacheError> {
        let kind = ImageType::from_content_type(content_type)?;
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES || !kind.matches_bytes(bytes) {
            return Err(ImageCacheError::InvalidImage);
        }
        let _guard = self
            .lock
            .lock()
            .map_err(|_| ImageCacheError::LockPoisoned)?;
        let target = self.path(key, kind);
        let temporary = self.root.join(format!(".{}.{}.tmp", key.0, Uuid::now_v7()));
        let written = (|| -> io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            // Windows rename cannot replace an existing file. Cache loss is
            // harmless; remove the old entry before publishing its replacement.
            if fs::symlink_metadata(&target).is_ok() {
                fs::remove_file(&target)?;
            }
            fs::rename(&temporary, &target)?;
            Ok(())
        })();
        if written.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        written?;
        for other in IMAGE_TYPES {
            if other != kind {
                let path = self.path(key, other);
                if fs::symlink_metadata(&path).is_ok() {
                    fs::remove_file(path)?;
                }
            }
        }
        self.evict_locked()?;
        Ok(())
    }

    fn path(&self, key: &ImageKey, kind: ImageType) -> PathBuf {
        self.root.join(format!("{}.{}", key.0, kind.extension()))
    }

    fn evict_locked(&self) -> Result<(), ImageCacheError> {
        let mut entries = Vec::new();
        let mut total = 0u64;
        for item in fs::read_dir(&self.root)? {
            let item = item?;
            let path = item.path();
            if !is_entry_name(&path) {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file() {
                fs::remove_file(path)?;
                continue;
            }
            total = total.saturating_add(metadata.len());
            entries.push((
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                path,
                metadata.len(),
            ));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        for (_, path, size) in entries {
            if total <= self.budget {
                break;
            }
            fs::remove_file(path)?;
            total = total.saturating_sub(size);
        }
        Ok(())
    }
}

fn is_entry_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    stem.len() == 64
        && stem.bytes().all(|b| b.is_ascii_hexdigit())
        && matches!(extension, "jpg" | "png" | "webp")
}

/// Serves only an opaque cache id. The custom protocol never fetches a remote
/// URL, follows a user-supplied path, or exposes a cache miss as a filesystem
/// error. Its Tauri callback runs this function on a blocking pool.
pub fn protocol_response(cache: &ImageCache, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    if request.method() != Method::GET {
        let mut response = empty_response(StatusCode::METHOD_NOT_ALLOWED);
        response
            .headers_mut()
            .insert(header::ALLOW, http::HeaderValue::from_static("GET"));
        return response;
    }
    let uri = request.uri();
    let valid_origin = matches!(
        (
            uri.scheme_str(),
            uri.authority().map(|authority| authority.host())
        ),
        (Some("vgimg"), Some("localhost")) | (Some("http"), Some("vgimg.localhost"))
    );
    let key = uri
        .path()
        .strip_prefix('/')
        .and_then(ImageKey::from_cache_id);
    if !valid_origin || uri.query().is_some() || key.is_none() {
        return empty_response(StatusCode::NOT_FOUND);
    }
    let Some(key) = key else {
        return empty_response(StatusCode::NOT_FOUND);
    };
    match cache.get(&key) {
        Ok(Some(image)) => {
            let mut response = Response::new(image.bytes);
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                http::HeaderValue::from_static(image.content_type),
            );
            secure_headers(&mut response);
            response
        }
        Ok(None) => empty_response(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(%error, "cannot read cached image");
            empty_response(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub fn empty_response(status: StatusCode) -> Response<Vec<u8>> {
    let mut response = Response::new(Vec::new());
    *response.status_mut() = status;
    secure_headers(&mut response);
    response
}

fn secure_headers(response: &mut Response<Vec<u8>>) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        http::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        http::HeaderValue::from_static("nosniff"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nvalid-for-cache-test";
    const WEBP: &[u8] = b"RIFF\x10\0\0\0WEBPvalid-for-cache-test";

    #[test]
    fn scopes_keys_to_server_and_never_uses_asset_text_as_a_path() {
        let server = Uuid::now_v7();
        let key = ImageKey::for_asset(server, "../cover?variant=wide").unwrap();
        assert_eq!(key.0.len(), 64);
        assert!(key.0.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(
            key,
            ImageKey::for_asset(Uuid::now_v7(), "../cover?variant=wide").unwrap()
        );
        assert!(ImageKey::for_asset(server, "").is_err());
    }

    #[test]
    fn accepts_only_matching_bounded_raster_images() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::open(dir.path().join("images")).unwrap();
        let key = ImageKey::for_asset(Uuid::now_v7(), "cover").unwrap();
        assert!(matches!(
            cache.insert(&key, "text/html", PNG),
            Err(ImageCacheError::UnsupportedType)
        ));
        assert!(matches!(
            cache.insert(&key, "image/svg+xml", PNG),
            Err(ImageCacheError::UnsupportedType)
        ));
        assert!(matches!(
            cache.insert(&key, "image/jpeg", PNG),
            Err(ImageCacheError::InvalidImage)
        ));
        assert!(matches!(
            cache.insert(&key, "image/png", &vec![0; MAX_IMAGE_BYTES + 1]),
            Err(ImageCacheError::InvalidImage)
        ));
        cache.insert(&key, "image/png", PNG).unwrap();
        assert_eq!(cache.get(&key).unwrap().unwrap().content_type, "image/png");
        let png_path = cache.path(&key, ImageType::Png);
        OpenOptions::new()
            .write(true)
            .open(&png_path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH)
            .unwrap();
        assert!(cache.get(&key).unwrap().is_some());
        assert!(fs::metadata(&png_path).unwrap().modified().unwrap() > SystemTime::UNIX_EPOCH);
        drop(cache);
        let cache = ImageCache::open(dir.path().join("images")).unwrap();
        assert_eq!(cache.get(&key).unwrap().unwrap().bytes, PNG);
        cache.insert(&key, "image/webp", WEBP).unwrap();
        assert_eq!(cache.get(&key).unwrap().unwrap().content_type, "image/webp");
        assert!(!cache.path(&key, ImageType::Png).exists());
    }

    #[test]
    fn evicts_oldest_and_never_serves_a_substituted_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::with_budget(
            dir.path().join("images"),
            PNG.len() as u64 + WEBP.len() as u64 - 1,
        )
        .unwrap();
        let server = Uuid::now_v7();
        let old = ImageKey::for_asset(server, "old").unwrap();
        let fresh = ImageKey::for_asset(server, "fresh").unwrap();
        cache.insert(&old, "image/png", PNG).unwrap();
        OpenOptions::new()
            .write(true)
            .open(cache.path(&old, ImageType::Png))
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH)
            .unwrap();
        cache.insert(&fresh, "image/webp", WEBP).unwrap();
        assert!(cache.get(&old).unwrap().is_none());
        assert!(cache.get(&fresh).unwrap().is_some());
        fs::write(cache.path(&old, ImageType::Png), PNG).unwrap();
        OpenOptions::new()
            .write(true)
            .open(cache.path(&old, ImageType::Png))
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH)
            .unwrap();
        drop(cache);
        let cache = ImageCache::with_budget(
            dir.path().join("images"),
            PNG.len() as u64 + WEBP.len() as u64 - 1,
        )
        .unwrap();
        assert!(cache.get(&old).unwrap().is_none());
        assert!(cache.get(&fresh).unwrap().is_some());
        fs::write(cache.path(&fresh, ImageType::Webp), b"<html>bad</html>").unwrap();
        assert!(cache.get(&fresh).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_served_or_used_as_a_cache_root() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.png");
        fs::write(&outside, PNG).unwrap();
        let root = dir.path().join("images");
        let cache = ImageCache::open(root.clone()).unwrap();
        let key = ImageKey::for_asset(Uuid::now_v7(), "cover").unwrap();
        symlink(&outside, cache.path(&key, ImageType::Png)).unwrap();
        assert!(cache.get(&key).unwrap().is_none());
        assert_eq!(fs::read(&outside).unwrap(), PNG);

        let alias = dir.path().join("alias");
        symlink(&root, &alias).unwrap();
        assert!(matches!(
            ImageCache::open(alias),
            Err(ImageCacheError::InvalidDirectory)
        ));
    }

    #[test]
    fn protocol_serves_only_gets_for_exact_opaque_local_urls() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::open(dir.path().join("images")).unwrap();
        let key = ImageKey::for_asset(Uuid::now_v7(), "cover").unwrap();
        cache.insert(&key, "image/png", PNG).unwrap();
        let request = |method, uri: String| {
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Vec::new())
                .unwrap()
        };
        for uri in [
            format!("vgimg://localhost/{}", key.0),
            format!("http://vgimg.localhost/{}", key.0),
        ] {
            let response = protocol_response(&cache, &request("GET", uri));
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.body(), PNG);
            assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        }
        for uri in [
            format!("vgimg://other/{}", key.0),
            format!("vgimg://localhost/{}?x=1", key.0),
            format!("vgimg://localhost/{}/extra", key.0),
            "vgimg://localhost/../outside".to_owned(),
            format!("https://vgimg.localhost/{}", key.0),
        ] {
            assert_eq!(
                protocol_response(&cache, &request("GET", uri)).status(),
                StatusCode::NOT_FOUND
            );
        }
        assert_eq!(
            protocol_response(
                &cache,
                &request("POST", format!("vgimg://localhost/{}", key.0))
            )
            .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
}
