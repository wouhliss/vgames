//! Catalog images (covers, heroes, logos, screenshots) behind `vgimg:` URLs.
//!
//! The catalog registers each asset it hands to the UI under its opaque cache
//! key. When the WebView asks for a key the cache does not hold, this fetches
//! `GET /v1/assets/{asset_id}` through the server's `ApiClient`, takes the
//! signed URL from the redirect without following it with the bearer token,
//! downloads at most 10 MiB without credentials, and stores the image only if
//! its bytes match a supported raster type (`images::ImageCache::insert`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use reqwest::StatusCode;
use uuid::Uuid;

use super::Connections;
use crate::api::{self, ApiError};
use crate::images::{CachedImage, ImageCache, ImageCacheError, ImageKey, MAX_IMAGE_BYTES};

/// Registered keys kept at most; the oldest registrations go first.
const MAX_KNOWN: usize = 8192;

#[derive(Debug, thiserror::Error)]
pub enum CoverError {
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error(transparent)]
    Catalog(#[from] super::CatalogError),
    #[error("the image server refused the download ({0})")]
    Status(u16),
    #[error("the image location is not allowed")]
    Location,
    #[error(transparent)]
    Cache(#[from] ImageCacheError),
    #[error("the image cache task failed")]
    Task,
}

pub struct Covers {
    cache: Arc<ImageCache>,
    /// Plain client for signed URLs: no credentials, no redirects, timeouts.
    http: reqwest::Client,
    known: Mutex<Known>,
}

#[derive(Default)]
struct Known {
    assets: HashMap<ImageKey, (Uuid, Uuid)>,
    order: std::collections::VecDeque<ImageKey>,
}

impl Covers {
    pub fn new(cache: Arc<ImageCache>, http: reqwest::Client) -> Self {
        Self {
            cache,
            http,
            known: Mutex::new(Known::default()),
        }
    }

    /// The `vgimg:` URL of a server's asset, which the protocol can now fetch.
    pub fn url(&self, server_id: Uuid, asset_id: Uuid) -> Option<String> {
        let key = ImageKey::for_asset(server_id, &asset_id.to_string()).ok()?;
        let url = key.url();
        if let Ok(mut known) = self.known.lock()
            && known
                .assets
                .insert(key.clone(), (server_id, asset_id))
                .is_none()
        {
            known.order.push_back(key);
            while known.order.len() > MAX_KNOWN {
                if let Some(old) = known.order.pop_front() {
                    known.assets.remove(&old);
                }
            }
        }
        Some(url)
    }

    fn lookup(&self, key: &ImageKey) -> Option<(Uuid, Uuid)> {
        self.known.lock().ok()?.assets.get(key).copied()
    }

    /// Forgets every registration (server switch, sign-out). Cached files
    /// stay: their keys are scoped to the server.
    pub fn forget_all(&self) {
        if let Ok(mut known) = self.known.lock() {
            *known = Known::default();
        }
    }

    /// The image for `key`: from the cache, or fetched when the catalog
    /// registered it. `None` for unknown keys.
    pub async fn load<C: Connections>(
        &self,
        connections: &C,
        key: ImageKey,
    ) -> Result<Option<CachedImage>, CoverError> {
        let cache = Arc::clone(&self.cache);
        let lookup_key = key.clone();
        if let Some(image) = tokio::task::spawn_blocking(move || cache.get(&lookup_key))
            .await
            .map_err(|_| CoverError::Task)??
        {
            return Ok(Some(image));
        }
        let Some((server_id, asset_id)) = self.lookup(&key) else {
            return Ok(None);
        };
        let client = connections.client(server_id).await?;
        let location = client
            .authed_location(&format!("v1/assets/{asset_id}"))
            .await?;
        // Signed URLs are https; plain http only from a plain-http (debug,
        // loopback) server.
        let allowed = location.scheme() == "https"
            || (location.scheme() == "http" && client.base().scheme() == "http");
        if !allowed || !location.username().is_empty() || location.password().is_some() {
            return Err(CoverError::Location);
        }
        let response = self
            .http
            .get(location)
            .send()
            .await
            .map_err(|e| ApiError::from_reqwest(&e))?;
        if response.status() != StatusCode::OK {
            return Err(CoverError::Status(response.status().as_u16()));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let bytes = api::read_capped(response, MAX_IMAGE_BYTES).await?;
        let cache = Arc::clone(&self.cache);
        tokio::task::spawn_blocking(move || {
            cache.insert(&key, &content_type, &bytes)?;
            cache.get(&key)
        })
        .await
        .map_err(|_| CoverError::Task)?
        .map_err(CoverError::from)
    }
}
