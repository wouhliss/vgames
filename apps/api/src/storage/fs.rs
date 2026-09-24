//! Local filesystem backend with HMAC-signed URLs served by the API under `/_storage/*`.
//!
//! Wire protocol (the subset of GCS XML API behaviour clients rely on):
//! - `GET /_storage/{bucket}/{object}?t=…` with optional `Range: bytes=a-b` → 200 / 206 / 416.
//! - `PUT /_storage/{bucket}/{object}?t=…` single upload; `Content-Type` and length must
//!   match the signature.
//! - `POST /_storage/{bucket}/{object}?t=…` with `x-goog-resumable: start` → `201` and a
//!   `Location` session URI.
//! - `PUT {session}` with `Content-Range: bytes a-b/total|*` → `308` + `Range: bytes=0-n`
//!   until complete, then `200`; `Content-Range: bytes */total|*` queries progress.
//!
//! Tokens are `base64url(claims).base64url(mac)` with HMAC-SHA-256 under a key derived
//! from `VGAMES_FS_URL_SIGNING_KEY`. Object names are validated before touching the disk
//! and every path is checked to stay under the root.

use std::{
    collections::HashMap,
    io::SeekFrom,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::{Path as AxumPath, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use super::{
    BucketKind, ByteRange, ByteStream, ObjectMeta, SignedRequest, Storage, StorageError,
    validate_name,
};
use crate::state::AppState;

/// How long a resumable session URI stays usable (GCS: one week).
pub const SESSION_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Op {
    Get,
    Put,
    ResumableStart,
    ResumableUpload,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Claims {
    op: Op,
    b: BucketKind,
    /// Object name, or the upload id for `ResumableUpload`.
    o: String,
    /// Unix expiry.
    e: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ct: Option<String>,
    /// Exact length (`Put`) or `[min, max]` (`ResumableStart`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    len: Option<(u64, u64)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct UploadMeta {
    bucket: BucketKind,
    name: String,
    min: u64,
    max: u64,
    received: u64,
    content_type: String,
}

pub struct FsStore {
    root: PathBuf,
    key: [u8; 32],
    origin: String,
    upload_locks: Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
}

impl FsStore {
    pub fn new(root: PathBuf, signing_key: &[u8], origin: String) -> Result<Self, StorageError> {
        std::fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        for b in BucketKind::ALL {
            std::fs::create_dir_all(root.join(b.as_str()))?;
        }
        std::fs::create_dir_all(root.join("_uploads"))?;
        Ok(Self {
            root,
            key: blake3::derive_key("vgames 2026-09 fs storage url v1", signing_key),
            origin,
            upload_locks: Mutex::new(HashMap::new()),
        })
    }

    fn object_path(&self, bucket: BucketKind, name: &str) -> Result<PathBuf, StorageError> {
        validate_name(name)?;
        let p = self.root.join(bucket.as_str()).join(name);
        // Validation already excludes `..`; keep a second guard against surprises.
        if !p.starts_with(self.root.join(bucket.as_str())) {
            return Err(StorageError::InvalidName);
        }
        Ok(p)
    }

    fn upload_dir(&self, id: Uuid) -> PathBuf {
        self.root.join("_uploads").join(id.to_string())
    }

    fn mac(&self, data: &[u8]) -> Vec<u8> {
        let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.key)
            .unwrap_or_else(|_| unreachable_hmac());
        m.update(data);
        m.finalize().into_bytes().to_vec()
    }

    fn token(&self, claims: &Claims) -> Result<String, StorageError> {
        let body = serde_json::to_vec(claims).map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&body),
            URL_SAFE_NO_PAD.encode(self.mac(&body))
        ))
    }

    fn verify(&self, token: &str) -> Option<Claims> {
        let (body, mac) = token.split_once('.')?;
        let body = URL_SAFE_NO_PAD.decode(body).ok()?;
        let mac = URL_SAFE_NO_PAD.decode(mac).ok()?;
        let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.key).ok()?;
        m.update(&body);
        m.verify_slice(&mac).ok()?;
        let claims: Claims = serde_json::from_slice(&body).ok()?;
        (claims.e > OffsetDateTime::now_utc().unix_timestamp()).then_some(claims)
    }

    fn signed(
        &self,
        claims: Claims,
        path: &str,
        method: &'static str,
        headers: Vec<(String, String)>,
    ) -> Result<SignedRequest, StorageError> {
        let expires_at = OffsetDateTime::from_unix_timestamp(claims.e)
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let t = self.token(&claims)?;
        Ok(SignedRequest {
            url: format!("{}{path}?t={t}", self.origin),
            method,
            headers,
            expires_at,
        })
    }

    fn expiry(ttl: Duration) -> i64 {
        OffsetDateTime::now_utc().unix_timestamp() + ttl.as_secs() as i64
    }

    pub fn sign_get(
        &self,
        bucket: BucketKind,
        name: &str,
        ttl: Duration,
    ) -> Result<SignedRequest, StorageError> {
        let claims = Claims {
            op: Op::Get,
            b: bucket,
            o: name.to_string(),
            e: Self::expiry(ttl),
            ct: None,
            len: None,
        };
        self.signed(
            claims,
            &format!("/_storage/{}/{name}", bucket.as_str()),
            "GET",
            Vec::new(),
        )
    }

    pub fn sign_put(
        &self,
        bucket: BucketKind,
        name: &str,
        ttl: Duration,
        content_type: &str,
        length: u64,
    ) -> Result<SignedRequest, StorageError> {
        let claims = Claims {
            op: Op::Put,
            b: bucket,
            o: name.to_string(),
            e: Self::expiry(ttl),
            ct: Some(content_type.to_string()),
            len: Some((length, length)),
        };
        let headers = vec![("content-type".to_string(), content_type.to_string())];
        self.signed(
            claims,
            &format!("/_storage/{}/{name}", bucket.as_str()),
            "PUT",
            headers,
        )
    }

    /// Single-shot PUT of `min..=max` bytes (length unknown when signing).
    pub fn sign_put_range(
        &self,
        bucket: BucketKind,
        name: &str,
        ttl: Duration,
        content_type: &str,
        min: u64,
        max: u64,
    ) -> Result<SignedRequest, StorageError> {
        let claims = Claims {
            op: Op::Put,
            b: bucket,
            o: name.to_string(),
            e: Self::expiry(ttl),
            ct: Some(content_type.to_string()),
            len: Some((min, max)),
        };
        let headers = vec![
            ("content-type".to_string(), content_type.to_string()),
            (
                "x-goog-content-length-range".to_string(),
                format!("{min},{max}"),
            ),
        ];
        self.signed(
            claims,
            &format!("/_storage/{}/{name}", bucket.as_str()),
            "PUT",
            headers,
        )
    }

    pub fn sign_resumable_start(
        &self,
        bucket: BucketKind,
        name: &str,
        ttl: Duration,
        content_type: &str,
        min: u64,
        max: u64,
    ) -> Result<SignedRequest, StorageError> {
        let claims = Claims {
            op: Op::ResumableStart,
            b: bucket,
            o: name.to_string(),
            e: Self::expiry(ttl),
            ct: Some(content_type.to_string()),
            len: Some((min, max)),
        };
        let headers = vec![
            ("content-type".to_string(), content_type.to_string()),
            ("x-goog-resumable".to_string(), "start".to_string()),
            (
                "x-goog-content-length-range".to_string(),
                format!("{min},{max}"),
            ),
        ];
        self.signed(
            claims,
            &format!("/_storage/{}/{name}", bucket.as_str()),
            "POST",
            headers,
        )
    }

    pub async fn head(
        &self,
        bucket: BucketKind,
        name: &str,
    ) -> Result<Option<ObjectMeta>, StorageError> {
        match tokio::fs::metadata(self.object_path(bucket, name)?).await {
            Ok(m) if m.is_file() => Ok(Some(ObjectMeta {
                size: m.len(),
                crc32c: None,
            })),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn get_range_stream(
        &self,
        bucket: BucketKind,
        name: &str,
        range: Option<ByteRange>,
    ) -> Result<ByteStream, StorageError> {
        let path = self.object_path(bucket, name)?;
        let mut file = match tokio::fs::File::open(&path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StorageError::NotFound);
            }
            Err(e) => return Err(e.into()),
        };
        let len = file.metadata().await?.len();
        let (start, count) = match range {
            Some(r) if r.start <= r.end && r.start < len => {
                (r.start, (r.end.min(len - 1)) - r.start + 1)
            }
            Some(_) => (0, 0),
            None => (0, len),
        };
        file.seek(SeekFrom::Start(start)).await?;
        let stream =
            tokio_util::io::ReaderStream::new(file.take(count)).map_err(StorageError::from);
        Ok(Box::pin(stream))
    }

    pub async fn put_small(
        &self,
        bucket: BucketKind,
        name: &str,
        data: Bytes,
    ) -> Result<(), StorageError> {
        let path = self.object_path(bucket, name)?;
        write_atomically(&self.root, &path, &data).await
    }

    pub async fn delete(&self, bucket: BucketKind, name: &str) -> Result<(), StorageError> {
        match tokio::fs::remove_file(self.object_path(bucket, name)?).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn list_prefix(
        &self,
        bucket: BucketKind,
        prefix: &str,
    ) -> Result<Vec<String>, StorageError> {
        let base = self.root.join(bucket.as_str());
        let mut out = Vec::new();
        let mut stack = vec![base.clone()];
        while let Some(dir) = stack.pop() {
            let mut rd = match tokio::fs::read_dir(&dir).await {
                Ok(rd) => rd,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            while let Some(entry) = rd.next_entry().await? {
                let ft = entry.file_type().await?;
                if ft.is_dir() {
                    stack.push(entry.path());
                } else if ft.is_file()
                    && let Ok(rel) = entry.path().strip_prefix(&base)
                {
                    let name = rel.to_string_lossy().replace('\\', "/");
                    if name.starts_with(prefix) && !name.contains(".vgtmp-") {
                        out.push(name);
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }

    fn lock_for(&self, id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .upload_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locks.entry(id).or_default().clone()
    }

    async fn read_meta(&self, id: Uuid) -> Option<UploadMeta> {
        let raw = tokio::fs::read(self.upload_dir(id).join("meta.json"))
            .await
            .ok()?;
        serde_json::from_slice(&raw).ok()
    }

    async fn write_meta(&self, id: Uuid, meta: &UploadMeta) -> Result<(), StorageError> {
        let raw = serde_json::to_vec(meta).map_err(|e| StorageError::Backend(e.to_string()))?;
        write_atomically(&self.root, &self.upload_dir(id).join("meta.json"), &raw).await
    }
}

#[allow(clippy::panic)]
fn unreachable_hmac() -> Hmac<Sha256> {
    // HMAC accepts keys of any length; this cannot happen.
    panic!("HMAC key rejected")
}

async fn write_atomically(root: &Path, path: &Path, data: &[u8]) -> Result<(), StorageError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = root
        .join("_uploads")
        .join(format!(".vgtmp-{}", Uuid::now_v7()));
    let mut f = tokio::fs::File::create(&tmp).await?;
    f.write_all(data).await?;
    f.sync_all().await?;
    drop(f);
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// HTTP routes (not part of the OpenAPI contract)
// ---------------------------------------------------------------------------------------

pub fn routes(state: &AppState) -> Router<AppState> {
    if matches!(*state.storage, Storage::Fs(_)) {
        Router::new()
            .route("/_storage/_uploads/{id}", put(upload_chunk))
            .route(
                "/_storage/{bucket}/{*name}",
                get(get_object).put(put_object).post(start_resumable),
            )
    } else {
        Router::new()
    }
}

#[derive(Deserialize)]
struct TokenQuery {
    t: Option<String>,
}

fn fs(state: &AppState) -> Option<&FsStore> {
    match &*state.storage {
        Storage::Fs(s) => Some(s),
        Storage::Gcs(_) => None,
    }
}

fn status(code: StatusCode, msg: &'static str) -> Response {
    (code, msg).into_response()
}

fn authorize(
    store: &FsStore,
    q: &TokenQuery,
    op: Op,
    bucket: &str,
    name: &str,
) -> Result<Claims, Box<Response>> {
    let denied = || Box::new(status(StatusCode::FORBIDDEN, "SignatureDoesNotMatch"));
    let claims =
        q.t.as_deref()
            .and_then(|t| store.verify(t))
            .ok_or_else(denied)?;
    if claims.op != op || claims.b.as_str() != bucket || claims.o != name {
        return Err(denied());
    }
    Ok(claims)
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn parse_range(v: &str) -> Option<(Option<u64>, Option<u64>)> {
    let spec = v.strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let a = if a.is_empty() {
        None
    } else {
        Some(a.parse().ok()?)
    };
    let b = if b.is_empty() {
        None
    } else {
        Some(b.parse().ok()?)
    };
    Some((a, b))
}

async fn get_object(
    State(state): State<AppState>,
    AxumPath((bucket, name)): AxumPath<(String, String)>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(store) = fs(&state) else {
        return status(StatusCode::NOT_FOUND, "");
    };
    if let Err(r) = authorize(store, &q, Op::Get, &bucket, &name) {
        return *r;
    }
    let Some(bk) = BucketKind::parse(&bucket) else {
        return status(StatusCode::NOT_FOUND, "NoSuchBucket");
    };
    let meta = match store.head(bk, &name).await {
        Ok(Some(m)) => m,
        Ok(None) => return status(StatusCode::NOT_FOUND, "NoSuchKey"),
        Err(StorageError::InvalidName) => return status(StatusCode::BAD_REQUEST, "InvalidName"),
        Err(_) => return status(StatusCode::INTERNAL_SERVER_ERROR, ""),
    };
    let len = meta.size;
    let (code, range) = match header_str(&headers, "range").map(parse_range) {
        None => (StatusCode::OK, None),
        Some(None) => return status(StatusCode::RANGE_NOT_SATISFIABLE, "InvalidRange"),
        Some(Some((start, end))) => {
            let (s, e) = match (start, end) {
                (Some(s), Some(e)) => (s, e.min(len.saturating_sub(1))),
                (Some(s), None) => (s, len.saturating_sub(1)),
                (None, Some(n)) => (len.saturating_sub(n), len.saturating_sub(1)),
                (None, None) => return status(StatusCode::RANGE_NOT_SATISFIABLE, "InvalidRange"),
            };
            if len == 0 || s > e || s >= len {
                let mut r = status(StatusCode::RANGE_NOT_SATISFIABLE, "InvalidRange");
                if let Ok(v) = HeaderValue::from_str(&format!("bytes */{len}")) {
                    r.headers_mut().insert(header::CONTENT_RANGE, v);
                }
                return r;
            }
            (
                StatusCode::PARTIAL_CONTENT,
                Some(ByteRange { start: s, end: e }),
            )
        }
    };
    let stream = match store.get_range_stream(bk, &name, range).await {
        Ok(s) => s,
        Err(_) => return status(StatusCode::INTERNAL_SERVER_ERROR, ""),
    };
    let mut resp = (code, Body::from_stream(stream)).into_response();
    let h = resp.headers_mut();
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    let body_len = range.map_or(len, |r| r.end - r.start + 1);
    if let Ok(v) = HeaderValue::from_str(&body_len.to_string()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    if let Some(r) = range
        && let Ok(v) = HeaderValue::from_str(&format!("bytes {}-{}/{len}", r.start, r.end))
    {
        h.insert(header::CONTENT_RANGE, v);
    }
    resp
}

/// Streams the request body to `tmp`, failing once more than `max` bytes arrive.
async fn stream_body_to(
    body: Body,
    file: &mut tokio::fs::File,
    max: u64,
) -> Result<u64, Box<Response>> {
    let mut stream = body.into_data_stream();
    let mut n: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|_| Box::new(status(StatusCode::BAD_REQUEST, "IncompleteBody")))?;
        n += chunk.len() as u64;
        if n > max {
            return Err(Box::new(status(StatusCode::BAD_REQUEST, "EntityTooLarge")));
        }
        file.write_all(&chunk)
            .await
            .map_err(|_| Box::new(status(StatusCode::INTERNAL_SERVER_ERROR, "")))?;
    }
    Ok(n)
}

async fn put_object(
    State(state): State<AppState>,
    AxumPath((bucket, name)): AxumPath<(String, String)>,
    Query(q): Query<TokenQuery>,
    req: Request,
) -> Response {
    let Some(store) = fs(&state) else {
        return status(StatusCode::NOT_FOUND, "");
    };
    let claims = match authorize(store, &q, Op::Put, &bucket, &name) {
        Ok(c) => c,
        Err(r) => return *r,
    };
    if header_str(req.headers(), "content-type") != claims.ct.as_deref() {
        return status(StatusCode::FORBIDDEN, "SignatureDoesNotMatch");
    }
    let (Some(bk), Some((min, max))) = (BucketKind::parse(&bucket), claims.len) else {
        return status(StatusCode::BAD_REQUEST, "");
    };
    let path = match store.object_path(bk, &name) {
        Ok(p) => p,
        Err(_) => return status(StatusCode::BAD_REQUEST, "InvalidName"),
    };
    let tmp = store
        .root
        .join("_uploads")
        .join(format!(".vgtmp-{}", Uuid::now_v7()));
    let Ok(mut file) = tokio::fs::File::create(&tmp).await else {
        return status(StatusCode::INTERNAL_SERVER_ERROR, "");
    };
    let written = match stream_body_to(req.into_body(), &mut file, max).await {
        Ok(n) => n,
        Err(r) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return *r;
        }
    };
    if written < min {
        let _ = tokio::fs::remove_file(&tmp).await;
        return status(StatusCode::BAD_REQUEST, "IncompleteBody");
    }
    let finalize = async {
        file.sync_all().await?;
        drop(file);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::rename(&tmp, &path).await
    };
    match finalize.await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(_) => status(StatusCode::INTERNAL_SERVER_ERROR, ""),
    }
}

async fn start_resumable(
    State(state): State<AppState>,
    AxumPath((bucket, name)): AxumPath<(String, String)>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(store) = fs(&state) else {
        return status(StatusCode::NOT_FOUND, "");
    };
    let claims = match authorize(store, &q, Op::ResumableStart, &bucket, &name) {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let Some((min, max)) = claims.len else {
        return status(StatusCode::BAD_REQUEST, "");
    };
    let range_header = format!("{min},{max}");
    if header_str(&headers, "x-goog-resumable") != Some("start")
        || header_str(&headers, "content-type") != claims.ct.as_deref()
        || header_str(&headers, "x-goog-content-length-range") != Some(range_header.as_str())
    {
        return status(StatusCode::FORBIDDEN, "SignatureDoesNotMatch");
    }
    let Some(bk) = BucketKind::parse(&bucket) else {
        return status(StatusCode::BAD_REQUEST, "");
    };
    let id = Uuid::now_v7();
    let meta = UploadMeta {
        bucket: bk,
        name,
        min,
        max,
        received: 0,
        content_type: claims.ct.unwrap_or_default(),
    };
    let dir = store.upload_dir(id);
    let create = async {
        tokio::fs::create_dir_all(&dir).await?;
        tokio::fs::File::create(dir.join("data")).await?;
        Ok::<(), std::io::Error>(())
    };
    if create.await.is_err() || store.write_meta(id, &meta).await.is_err() {
        return status(StatusCode::INTERNAL_SERVER_ERROR, "");
    }
    let session = Claims {
        op: Op::ResumableUpload,
        b: bk,
        o: id.to_string(),
        e: FsStore::expiry(SESSION_TTL),
        ct: None,
        len: None,
    };
    let Ok(t) = store.token(&session) else {
        return status(StatusCode::INTERNAL_SERVER_ERROR, "");
    };
    let location = format!("{}/_storage/_uploads/{id}?t={t}", store.origin);
    let mut resp = StatusCode::CREATED.into_response();
    if let Ok(v) = HeaderValue::from_str(&location) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    resp
}

/// `bytes a-b/total`, `bytes a-b/*`, `bytes */total` or `bytes */*`.
/// `(written range, declared total)` of a `Content-Range` header.
type ContentRange = (Option<(u64, u64)>, Option<u64>);

fn parse_content_range(v: &str) -> Option<ContentRange> {
    let spec = v.strip_prefix("bytes ")?;
    let (range, total) = spec.split_once('/')?;
    let total = if total == "*" {
        None
    } else {
        Some(total.parse().ok()?)
    };
    let range = if range == "*" {
        None
    } else {
        let (a, b) = range.split_once('-')?;
        let (a, b): (u64, u64) = (a.parse().ok()?, b.parse().ok()?);
        if b < a {
            return None;
        }
        Some((a, b))
    };
    Some((range, total))
}

fn incomplete(received: u64) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::PERMANENT_REDIRECT; // 308 "Resume Incomplete"
    if received > 0
        && let Ok(v) = HeaderValue::from_str(&format!("bytes=0-{}", received - 1))
    {
        resp.headers_mut().insert(header::RANGE, v);
    }
    resp
}

async fn upload_chunk(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<TokenQuery>,
    req: Request,
) -> Response {
    let Some(store) = fs(&state) else {
        return status(StatusCode::NOT_FOUND, "");
    };
    if authorize(store, &q, Op::ResumableUpload, "packages", &id).is_err()
        && authorize(store, &q, Op::ResumableUpload, "saves", &id).is_err()
        && authorize(store, &q, Op::ResumableUpload, "assets", &id).is_err()
    {
        return status(StatusCode::FORBIDDEN, "SignatureDoesNotMatch");
    }
    let Ok(id) = Uuid::parse_str(&id) else {
        return status(StatusCode::NOT_FOUND, "");
    };
    let lock = store.lock_for(id);
    let _guard = lock.lock().await;
    let Some(mut meta) = store.read_meta(id).await else {
        return status(StatusCode::NOT_FOUND, "NoSuchUpload");
    };

    let Some((range, total)) =
        header_str(req.headers(), "content-range").and_then(parse_content_range)
    else {
        return status(StatusCode::BAD_REQUEST, "InvalidContentRange");
    };
    if let Some(t) = total
        && (t > meta.max || t < meta.min)
    {
        return status(StatusCode::BAD_REQUEST, "EntityTooLarge");
    }

    if let Some((a, b)) = range {
        if a > meta.received {
            return status(StatusCode::BAD_REQUEST, "NonContiguousChunk");
        }
        if b + 1 > meta.max || total.is_some_and(|t| b + 1 > t) {
            return status(StatusCode::BAD_REQUEST, "EntityTooLarge");
        }
        let data_path = store.upload_dir(id).join("data");
        let Ok(mut file) = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&data_path)
            .await
        else {
            return status(StatusCode::INTERNAL_SERVER_ERROR, "");
        };
        if file.seek(SeekFrom::Start(a)).await.is_err() {
            return status(StatusCode::INTERNAL_SERVER_ERROR, "");
        }
        let expected = b - a + 1;
        let written = match stream_body_to(req.into_body(), &mut file, expected).await {
            Ok(n) => n,
            Err(r) => return *r,
        };
        if written != expected {
            return status(StatusCode::BAD_REQUEST, "IncompleteBody");
        }
        if file.sync_all().await.is_err() {
            return status(StatusCode::INTERNAL_SERVER_ERROR, "");
        }
        meta.received = meta.received.max(b + 1);
        if store.write_meta(id, &meta).await.is_err() {
            return status(StatusCode::INTERNAL_SERVER_ERROR, "");
        }
    }

    match total {
        Some(t) if meta.received == t => {
            let Ok(path) = store.object_path(meta.bucket, &meta.name) else {
                return status(StatusCode::BAD_REQUEST, "");
            };
            let dir = store.upload_dir(id);
            let finish = async {
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::rename(dir.join("data"), &path).await?;
                tokio::fs::remove_dir_all(&dir).await
            };
            if finish.await.is_err() {
                return status(StatusCode::INTERNAL_SERVER_ERROR, "");
            }
            let body = serde_json::json!({ "name": meta.name, "size": t.to_string(), "contentType": meta.content_type });
            (StatusCode::OK, axum::Json(body)).into_response()
        }
        _ => incomplete(meta.received),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_forms() {
        assert_eq!(
            parse_content_range("bytes 0-99/200"),
            Some((Some((0, 99)), Some(200)))
        );
        assert_eq!(
            parse_content_range("bytes 100-199/*"),
            Some((Some((100, 199)), None))
        );
        assert_eq!(parse_content_range("bytes */200"), Some((None, Some(200))));
        assert_eq!(parse_content_range("bytes */*"), Some((None, None)));
        assert_eq!(parse_content_range("bytes 9-1/*"), None);
        assert_eq!(parse_content_range("items 0-1/2"), None);
    }

    #[test]
    fn range_forms() {
        assert_eq!(parse_range("bytes=0-9"), Some((Some(0), Some(9))));
        assert_eq!(parse_range("bytes=5-"), Some((Some(5), None)));
        assert_eq!(parse_range("bytes=-5"), Some((None, Some(5))));
        assert_eq!(parse_range("bytes=0-1,4-5"), None);
    }
}
