//! Upload test support: a loopback storage server speaking the resumable
//! upload protocol exactly like GCS and the API's `fs` backend (start →
//! `201 Location`; `PUT` with `Content-Range` → `308 Range: bytes=0-n` or
//! `200`; `bytes */total` status queries), with fault injection, and a mock
//! of the admin API whose `finalize` does what the server does: fetch the
//! manifest, check size and hash, `verify_manifest` in server mode, check
//! every pack object, then verify every pack with `PackStreamVerifier`.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_core::verify::{ExpectedRelease, VerifyMode, verify_manifest};
use vgames_core::{Digest, Envelope, Timestamp};
use vgames_pack::verify::{PackExpectation, PackStreamVerifier};
use vgames_proto::auth::UserPublic;
use vgames_proto::versions::{
    FinalizeRequest, SignatureEnvelope, UploadMethod, UploadTarget, Version, VersionCreate,
    VersionState,
};

use super::package::{ADMIN_ID, SERVER_ID, trust_state};
use crate::download::RemoteError;
use crate::upload::PublishApi;

/// A failure injected into the next data `PUT` of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadFault {
    /// Read this many body bytes, then close without answering (nothing stored).
    DropMidBody { after: u64 },
    /// Read the body, store nothing, answer this status.
    Status(u16),
    /// Store only the first `keep` bytes of the piece and answer `308` (GCS may
    /// persist less than it received).
    PartialCommit { keep: u64 },
    /// The session is forgotten (expired): `404`.
    LoseSession,
    /// Read the body, then answer nothing for this long.
    Stall(Duration),
}

#[derive(Debug)]
struct Session {
    object: String,
    data: Vec<u8>,
    max: u64,
}

#[derive(Default)]
struct State {
    /// Start tokens the API issued: token → (object, content type).
    tokens: Mutex<HashMap<String, (String, String)>>,
    sessions: Mutex<HashMap<String, Session>>,
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    faults: Mutex<VecDeque<UploadFault>>,
    data_puts: AtomicU64,
    data_bytes: AtomicU64,
    status_queries: AtomicU64,
    sessions_started: AtomicU64,
    /// Pause per data PUT (slows uploads for kill tests).
    put_delay_us: AtomicU64,
    /// Forget every session once this many data PUTs were stored (0: never).
    lose_sessions_after: AtomicU64,
}

/// The storage server. Dropping it stops it.
pub struct UploadRig {
    pub addr: SocketAddr,
    state: Arc<State>,
    shutdown: CancellationToken,
}

impl Drop for UploadRig {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl UploadRig {
    pub async fn start() -> Arc<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(State::default());
        let shutdown = CancellationToken::new();
        let (st, stop) = (Arc::clone(&state), shutdown.clone());
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    () = stop.cancelled() => return,
                    a = listener.accept() => a,
                };
                let Ok((stream, _)) = accepted else { continue };
                let (st, stop) = (Arc::clone(&st), stop.clone());
                tokio::spawn(async move {
                    tokio::select! {
                        () = stop.cancelled() => {}
                        () = serve(stream, st, addr) => {}
                    }
                });
            }
        });
        Arc::new(Self {
            addr,
            state,
            shutdown,
        })
    }

    fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// A signed-looking resumable start target for `object`.
    pub fn start_target(&self, object: &str) -> UploadTarget {
        let token = Uuid::now_v7().simple().to_string();
        self.state.tokens.lock().unwrap().insert(
            token.clone(),
            (object.to_owned(), "application/octet-stream".into()),
        );
        UploadTarget {
            url: format!("{}/start?t={token}", self.base()),
            method: UploadMethod::Post,
            headers: BTreeMap::from([
                ("content-type".into(), "application/octet-stream".into()),
                ("x-goog-resumable".into(), "start".into()),
                ("x-goog-content-length-range".into(), "1,268435456".into()),
            ]),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(15),
        }
    }

    /// A signed-looking single PUT target for `object`.
    pub fn put_target(&self, object: &str) -> UploadTarget {
        let token = Uuid::now_v7().simple().to_string();
        self.state.tokens.lock().unwrap().insert(
            token.clone(),
            (object.to_owned(), "application/json".into()),
        );
        UploadTarget {
            url: format!("{}/object?t={token}", self.base()),
            method: UploadMethod::Put,
            headers: BTreeMap::from([
                ("content-type".into(), "application/json".into()),
                ("x-goog-content-length-range".into(), "1,268435456".into()),
            ]),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(15),
        }
    }

    pub fn inject(&self, fault: UploadFault) {
        self.state.faults.lock().unwrap().push_back(fault);
    }

    pub fn pending_faults(&self) -> usize {
        self.state.faults.lock().unwrap().len()
    }

    /// Every start token issued so far stops working (403).
    pub fn expire_start_tokens(&self) {
        self.state.tokens.lock().unwrap().clear();
    }

    /// Forgets every open session (as if they expired): later requests get 404.
    pub fn drop_sessions(&self) {
        self.state.sessions.lock().unwrap().clear();
    }

    /// Forgets every open session once `puts` data PUTs were stored
    /// (deterministic expiry in the middle of an upload).
    pub fn drop_sessions_after(&self, puts: u64) {
        self.state.lose_sessions_after.store(puts, Ordering::SeqCst);
    }

    pub fn set_put_delay(&self, delay: Duration) {
        self.state
            .put_delay_us
            .store(delay.as_micros() as u64, Ordering::Relaxed);
    }

    pub fn object(&self, name: &str) -> Option<Vec<u8>> {
        self.state.objects.lock().unwrap().get(name).cloned()
    }

    pub fn object_names(&self) -> Vec<String> {
        self.state.objects.lock().unwrap().keys().cloned().collect()
    }

    /// Body bytes accepted by data PUTs (the network cost of the uploads).
    pub fn data_bytes(&self) -> u64 {
        self.state.data_bytes.load(Ordering::Relaxed)
    }

    pub fn data_puts(&self) -> u64 {
        self.state.data_puts.load(Ordering::Relaxed)
    }

    pub fn status_queries(&self) -> u64 {
        self.state.status_queries.load(Ordering::Relaxed)
    }

    pub fn sessions_started(&self) -> u64 {
        self.state.sessions_started.load(Ordering::Relaxed)
    }
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

async fn read_head(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> Option<Request> {
    let mut line = String::new();
    if reader.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).await.ok()? == 0 {
            return None;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    Some(Request {
        method,
        path,
        headers,
    })
}

async fn reply(
    write: &mut tokio::net::tcp::OwnedWriteHalf,
    status: &str,
    headers: &[(&str, String)],
) -> bool {
    let mut text = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n");
    for (k, v) in headers {
        text.push_str(&format!("{k}: {v}\r\n"));
    }
    text.push_str("\r\n");
    write.write_all(text.as_bytes()).await.is_ok()
}

async fn serve(stream: TcpStream, state: Arc<State>, addr: SocketAddr) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    while let Some(request) = read_head(&mut reader).await {
        let length: u64 = request
            .headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let (path, query) = request.path.split_once('?').unwrap_or((&request.path, ""));
        let token = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("t="))
            .unwrap_or_default()
            .to_owned();
        let keep_going = match (request.method.as_str(), path) {
            ("POST", "/start") => {
                if !read_body(&mut reader, length).await.is_some() {
                    return;
                }
                start(&request, &token, &state, &mut write, addr).await
            }
            ("PUT", "/object") => {
                let Some(body) = read_body(&mut reader, length).await else {
                    return;
                };
                put_object(&request, &token, body, &state, &mut write).await
            }
            ("PUT", p) if p.starts_with("/upload/") => {
                let id = p.trim_start_matches("/upload/").to_owned();
                data_put(&request, &id, length, &mut reader, &mut write, &state).await
            }
            _ => {
                let _ = read_body(&mut reader, length).await;
                reply(&mut write, "404 Not Found", &[]).await
            }
        };
        if !keep_going {
            return;
        }
    }
}

async fn read_body(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    length: u64,
) -> Option<Vec<u8>> {
    let mut body = vec![0u8; length as usize];
    reader.read_exact(&mut body).await.ok()?;
    Some(body)
}

async fn start(
    request: &Request,
    token: &str,
    state: &State,
    write: &mut tokio::net::tcp::OwnedWriteHalf,
    addr: SocketAddr,
) -> bool {
    let issued = state.tokens.lock().unwrap().get(token).cloned();
    let Some((object, content_type)) = issued else {
        return reply(write, "403 Forbidden", &[]).await;
    };
    let h = |k: &str| request.headers.get(k).map(String::as_str);
    if h("x-goog-resumable") != Some("start")
        || h("content-type") != Some(content_type.as_str())
        || h("x-goog-content-length-range") != Some("1,268435456")
    {
        return reply(write, "403 Forbidden", &[]).await;
    }
    let id = Uuid::now_v7().simple().to_string();
    state.sessions.lock().unwrap().insert(
        id.clone(),
        Session {
            object,
            data: Vec::new(),
            max: 268_435_456,
        },
    );
    state.sessions_started.fetch_add(1, Ordering::Relaxed);
    reply(
        write,
        "201 Created",
        &[("Location", format!("http://{addr}/upload/{id}"))],
    )
    .await
}

async fn put_object(
    request: &Request,
    token: &str,
    body: Vec<u8>,
    state: &State,
    write: &mut tokio::net::tcp::OwnedWriteHalf,
) -> bool {
    let issued = state.tokens.lock().unwrap().get(token).cloned();
    let Some((object, content_type)) = issued else {
        return reply(write, "403 Forbidden", &[]).await;
    };
    if request.headers.get("content-type") != Some(&content_type)
        || request
            .headers
            .get("x-goog-content-length-range")
            .map(String::as_str)
            != Some("1,268435456")
        || body.is_empty()
    {
        return reply(write, "403 Forbidden", &[]).await;
    }
    state.objects.lock().unwrap().insert(object, body);
    reply(write, "200 OK", &[]).await
}

/// `(written range, declared total)` of a `Content-Range` header.
type ContentRange = (Option<(u64, u64)>, Option<u64>);

/// `bytes a-b/total`, `bytes */total` (and `*` totals).
fn content_range(v: &str) -> Option<ContentRange> {
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
        Some((a.parse().ok()?, b.parse().ok()?))
    };
    Some((range, total))
}

fn progress(received: u64) -> Vec<(&'static str, String)> {
    if received == 0 {
        vec![]
    } else {
        vec![("Range", format!("bytes=0-{}", received - 1))]
    }
}

async fn data_put(
    request: &Request,
    id: &str,
    length: u64,
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    write: &mut tokio::net::tcp::OwnedWriteHalf,
    state: &State,
) -> bool {
    let Some((range, total)) = request
        .headers
        .get("content-range")
        .and_then(|v| content_range(v))
    else {
        let _ = read_body(reader, length).await;
        return reply(write, "400 Bad Request", &[]).await;
    };
    let fault = if range.is_some() {
        state.faults.lock().unwrap().pop_front()
    } else {
        None
    };
    if range.is_none() {
        state.status_queries.fetch_add(1, Ordering::Relaxed);
    }
    if let Some(UploadFault::DropMidBody { after }) = fault {
        let mut sink = vec![0u8; after.min(length) as usize];
        let _ = reader.read_exact(&mut sink).await;
        return false;
    }
    let Some(body) = read_body(reader, length).await else {
        return false;
    };
    let delay = state.put_delay_us.load(Ordering::Relaxed);
    if delay > 0 && range.is_some() {
        tokio::time::sleep(Duration::from_micros(delay)).await;
    }
    match fault {
        Some(UploadFault::Status(code)) => {
            return reply(write, &format!("{code} Injected"), &[]).await;
        }
        Some(UploadFault::LoseSession) => {
            state.sessions.lock().unwrap().remove(id);
        }
        Some(UploadFault::Stall(d)) => {
            tokio::time::sleep(d).await;
            return false;
        }
        _ => {}
    }
    let (status, headers) = commit(state, id, range, total, &body, fault.as_ref());
    reply(write, status, &headers).await
}

/// Applies a data PUT to the session (no awaits while locked).
fn commit(
    state: &State,
    id: &str,
    range: Option<(u64, u64)>,
    total: Option<u64>,
    body: &[u8],
    fault: Option<&UploadFault>,
) -> (&'static str, Vec<(&'static str, String)>) {
    let mut sessions = state.sessions.lock().unwrap();
    let Some(session) = sessions.get_mut(id) else {
        return ("404 Not Found", vec![]);
    };
    if let Some((a, b)) = range {
        let received = session.data.len() as u64;
        if a > received || b < a || b - a + 1 != body.len() as u64 || b + 1 > session.max {
            return ("400 Bad Request", vec![]);
        }
        let keep = match fault {
            Some(UploadFault::PartialCommit { keep }) => (*keep).min(body.len() as u64),
            _ => body.len() as u64,
        };
        // A resent piece overlaps what is stored.
        let skip = received - a;
        if skip < keep {
            session
                .data
                .extend_from_slice(&body[skip as usize..keep as usize]);
        }
        let puts = state.data_puts.fetch_add(1, Ordering::SeqCst) + 1;
        state
            .data_bytes
            .fetch_add(body.len() as u64, Ordering::Relaxed);
        if puts == state.lose_sessions_after.load(Ordering::SeqCst) {
            sessions.clear();
            return ("404 Not Found", vec![]);
        }
    }
    let received = session.data.len() as u64;
    if total == Some(received) {
        let session = sessions.remove(id).unwrap();
        drop(sessions);
        state
            .objects
            .lock()
            .unwrap()
            .insert(session.object, session.data);
        return ("200 OK", vec![]);
    }
    ("308 Resume Incomplete", progress(received))
}

#[derive(Debug, Clone)]
struct VersionRec {
    version: Version,
}

/// Mock of the admin API used by publishes.
pub struct MockPublishApi {
    rig: Arc<UploadRig>,
    versions: Mutex<HashMap<Uuid, VersionRec>>,
    signatures: Mutex<HashMap<Uuid, SignatureEnvelope>>,
    next_sequence: AtomicU64,
    pub create_calls: AtomicU64,
    pub finalize_calls: AtomicU64,
    /// Errors returned by the next calls (any method).
    pub errors: Mutex<VecDeque<RemoteError>>,
    /// Who the server thinks is calling (the key holder must match).
    pub caller: Mutex<Uuid>,
}

impl MockPublishApi {
    pub fn new(rig: Arc<UploadRig>) -> Arc<Self> {
        Arc::new(Self {
            rig,
            versions: Mutex::new(HashMap::new()),
            signatures: Mutex::new(HashMap::new()),
            next_sequence: AtomicU64::new(1),
            create_calls: AtomicU64::new(0),
            finalize_calls: AtomicU64::new(0),
            errors: Mutex::new(VecDeque::new()),
            caller: Mutex::new(ADMIN_ID),
        })
    }

    fn injected(&self) -> Result<(), RemoteError> {
        match self.errors.lock().unwrap().pop_front() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    pub fn version(&self, id: Uuid) -> Option<Version> {
        self.versions
            .lock()
            .unwrap()
            .get(&id)
            .map(|r| r.version.clone())
    }

    /// The signature envelope accepted at finalize.
    pub fn signature(&self, id: Uuid) -> Option<vgames_core::Envelope> {
        let wire = self.signatures.lock().unwrap().get(&id).cloned()?;
        vgames_core::Envelope::parse(&serde_json::to_vec(&wire).unwrap()).ok()
    }

    fn prefix(version: &Version) -> String {
        format!("v1/{}/{}", version.package_id, version.id)
    }

    /// Object names of a version's packs and manifest.
    pub fn pack_object(version: &Version, pack: u32) -> String {
        format!("{}/packs/{pack:05}.pack", Self::prefix(version))
    }

    pub fn manifest_object(version: &Version) -> String {
        format!("{}/manifest.json", Self::prefix(version))
    }

    fn get(&self, id: Uuid) -> Result<Version, RemoteError> {
        self.version(id).ok_or_else(|| RemoteError {
            retryable: false,
            code: Some("not_found".into()),
            message: "no such version".into(),
        })
    }

    fn set(&self, version: Version) {
        self.versions
            .lock()
            .unwrap()
            .insert(version.id, VersionRec { version });
    }

    fn conflict(message: &str) -> RemoteError {
        RemoteError {
            retryable: false,
            code: Some("conflict".into()),
            message: message.into(),
        }
    }

    fn unprocessable(code: &str, message: String) -> RemoteError {
        RemoteError {
            retryable: false,
            code: Some(code.into()),
            message,
        }
    }

    /// The server side of finalize (A1-T11) plus the verify job (A1-T12).
    fn check_upload(
        &self,
        version: &Version,
        request: &FinalizeRequest,
    ) -> Result<Option<String>, RemoteError> {
        let manifest = self
            .rig
            .object(&Self::manifest_object(version))
            .ok_or_else(|| Self::unprocessable("manifest_missing", "no manifest.json".into()))?;
        if manifest.len() as i64 != request.manifest_size
            || Digest::of(&manifest).to_hex() != request.manifest_blake3
        {
            return Err(Self::unprocessable(
                "manifest_hash_mismatch",
                "size or hash".into(),
            ));
        }
        let envelope_json = serde_json::to_vec(&request.signature).unwrap();
        let envelope = Envelope::parse(&envelope_json)
            .map_err(|e| Self::unprocessable("signature_invalid", e.to_string()))?;
        let platform: vgames_core::manifest::Platform =
            serde_json::from_value(serde_json::to_value(version.platform).unwrap()).unwrap();
        let verified = verify_manifest(
            &trust_state(),
            &envelope,
            &manifest,
            &ExpectedRelease {
                server_id: version.server_id,
                package_id: version.package_id,
                version_id: version.id,
                platform,
                sequence: version.sequence as u64,
            },
            None,
            VerifyMode::Server {
                now: Timestamp::new(time::OffsetDateTime::now_utc()),
                caller: *self.caller.lock().unwrap(),
            },
        )
        .map_err(|e| Self::unprocessable("manifest_invalid", e.to_string()))?;
        for (i, pack) in verified.manifest.packs.iter().enumerate() {
            let bytes = self
                .rig
                .object(&Self::pack_object(version, i as u32))
                .ok_or_else(|| Self::unprocessable("pack_missing", format!("pack {i}")))?;
            if bytes.len() as u64 != pack.size {
                return Err(Self::unprocessable(
                    "pack_size_mismatch",
                    format!("pack {i}"),
                ));
            }
        }
        // The verify job: decode and hash every chunk, check pack hashes.
        for i in 0..verified.manifest.packs.len() as u32 {
            let bytes = self.rig.object(&Self::pack_object(version, i)).unwrap();
            let mut verifier =
                PackStreamVerifier::new(PackExpectation::from_manifest(&manifest, i).unwrap());
            let mut bad = None;
            for piece in bytes.chunks(1_000_003) {
                verifier
                    .push(piece, |v| {
                        if v.result.is_err() && bad.is_none() {
                            bad = Some(v.index);
                        }
                    })
                    .map_err(|e| Self::unprocessable("pack_invalid", e.to_string()))?;
            }
            if let Err(e) = verifier.finish() {
                return Ok(Some(format!("pack {i}: {e}")));
            }
            if let Some(chunk) = bad {
                return Ok(Some(format!("pack {i} chunk {chunk} is damaged")));
            }
        }
        Ok(None)
    }
}

impl PublishApi for MockPublishApi {
    async fn create_version(
        &self,
        package_id: Uuid,
        request: VersionCreate,
    ) -> Result<Version, RemoteError> {
        self.injected()?;
        self.create_calls.fetch_add(1, Ordering::Relaxed);
        let version = Version {
            id: Uuid::now_v7(),
            package_id,
            server_id: SERVER_ID,
            platform: request.platform,
            sequence: self.next_sequence.fetch_add(1, Ordering::Relaxed) as i64,
            version_label: request.version_label,
            state: VersionState::Uploading,
            is_current_release: None,
            failure_reason: None,
            total_size: None,
            file_count: None,
            chunk_count: None,
            pack_count: None,
            publisher_key_id: None,
            verify_progress: None,
            created_at: time::OffsetDateTime::now_utc(),
            created_by: UserPublic {
                id: ADMIN_ID,
                username: "admin".into(),
                display_name: None,
                avatar_url: None,
            },
            finalized_at: None,
            verified_at: None,
            published_at: None,
            yanked_at: None,
        };
        self.set(version.clone());
        Ok(version)
    }

    async fn get_version(&self, version_id: Uuid) -> Result<Version, RemoteError> {
        self.injected()?;
        let mut version = self.get(version_id)?;
        // The verify job finishes between two polls.
        if version.state == VersionState::Verifying {
            version.state = VersionState::Ready;
            version.verify_progress = Some(1.0);
            self.set(version.clone());
        }
        Ok(version)
    }

    async fn pack_upload_session(
        &self,
        version_id: Uuid,
        pack: u32,
    ) -> Result<UploadTarget, RemoteError> {
        self.injected()?;
        let version = self.get(version_id)?;
        if version.state != VersionState::Uploading {
            return Err(Self::conflict("not uploading"));
        }
        Ok(self.rig.start_target(&Self::pack_object(&version, pack)))
    }

    async fn manifest_upload(&self, version_id: Uuid) -> Result<UploadTarget, RemoteError> {
        self.injected()?;
        let version = self.get(version_id)?;
        if version.state != VersionState::Uploading {
            return Err(Self::conflict("not uploading"));
        }
        Ok(self.rig.put_target(&Self::manifest_object(&version)))
    }

    async fn finalize(
        &self,
        version_id: Uuid,
        request: FinalizeRequest,
    ) -> Result<Version, RemoteError> {
        self.injected()?;
        self.finalize_calls.fetch_add(1, Ordering::Relaxed);
        let mut version = self.get(version_id)?;
        if version.state != VersionState::Uploading {
            return Err(Self::conflict("not uploading"));
        }
        let failure = self.check_upload(&version, &request)?;
        self.signatures
            .lock()
            .unwrap()
            .insert(version_id, request.signature.clone());
        version.finalized_at = Some(time::OffsetDateTime::now_utc());
        match failure {
            None => {
                version.state = VersionState::Verifying;
                version.verify_progress = Some(0.0);
            }
            Some(reason) => {
                version.state = VersionState::Failed;
                version.failure_reason = Some(reason);
            }
        }
        self.set(version.clone());
        Ok(version)
    }

    async fn publish(&self, version_id: Uuid) -> Result<Version, RemoteError> {
        self.injected()?;
        let mut version = self.get(version_id)?;
        if version.state != VersionState::Ready {
            return Err(Self::conflict("not ready"));
        }
        version.state = VersionState::Published;
        version.published_at = Some(time::OffsetDateTime::now_utc());
        self.set(version.clone());
        Ok(version)
    }

    async fn abort(&self, version_id: Uuid) -> Result<(), RemoteError> {
        self.injected()?;
        let mut version = self.get(version_id)?;
        version.state = VersionState::Aborted;
        self.set(version);
        Ok(())
    }
}
