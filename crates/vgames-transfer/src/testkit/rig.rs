//! A loopback "object storage" for transfer tests: a small HTTP/1.1 server
//! (keep-alive, `Range` requests) serving a manifest and packs from memory,
//! with fault injection: dropped connections, 5xx, wrong `Content-Range`,
//! truncated bodies, stalls, expired links and corrupted bytes.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// One injected failure, applied to the next matching pack request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// Send the headers and this many body bytes, then close the connection.
    DropMidBody { after: u64 },
    /// Answer with this status and an empty body.
    Status(u16),
    /// A `206` whose `Content-Range` names other bytes.
    WrongContentRange,
    /// A `206` with the right `Content-Range` but a shorter body and
    /// `Content-Length` (a well-formed but truncated response).
    Truncated,
    /// Send some bytes, then nothing for this long, then close.
    Stall(Duration),
    /// `200` with the whole object instead of a range.
    IgnoreRange,
}

#[derive(Default)]
struct State {
    manifest: Mutex<Vec<u8>>,
    packs: Mutex<Vec<Arc<Vec<u8>>>>,
    faults: Mutex<VecDeque<(Option<u32>, Fault)>>,
    /// Persistent corruption: (pack, offset) bytes are served flipped.
    flips: Mutex<Vec<(u32, u64)>>,
    /// Links with a lower token are expired (403).
    min_token: AtomicU64,
    token: AtomicU64,
    /// Pause after each 1 MiB piece (slows downloads for kill tests).
    piece_delay_us: AtomicU64,
    pack_requests: AtomicU64,
    /// Expire every link when this many pack requests were served (0: never).
    expire_after: AtomicU64,
    pack_bytes: AtomicU64,
    connections: AtomicU64,
    ranges: Mutex<Vec<(u32, u64, u64)>>,
}

/// The running server. Dropping it stops accepting connections.
pub struct Rig {
    pub addr: SocketAddr,
    state: Arc<State>,
    shutdown: CancellationToken,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl Rig {
    pub async fn start(manifest: Vec<u8>, packs: Vec<Arc<Vec<u8>>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(State::default());
        *state.manifest.lock().unwrap() = manifest;
        *state.packs.lock().unwrap() = packs;
        state.token.store(1, Ordering::SeqCst);
        let shutdown = CancellationToken::new();
        let (st, stop) = (Arc::clone(&state), shutdown.clone());
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    () = stop.cancelled() => return,
                    a = listener.accept() => a,
                };
                let Ok((stream, _)) = accepted else { continue };
                let _ = stream.set_nodelay(true);
                st.connections.fetch_add(1, Ordering::Relaxed);
                let (st, stop) = (Arc::clone(&st), stop.clone());
                tokio::spawn(async move {
                    tokio::select! {
                        () = stop.cancelled() => {}
                        () = serve(stream, st) => {}
                    }
                });
            }
        });
        Self {
            addr,
            state,
            shutdown,
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn manifest_url(&self) -> String {
        format!("{}/manifest.json?sig=secret", self.base_url())
    }

    /// A signed-looking link for a pack with the current token.
    pub fn pack_url(&self, pack: u32) -> String {
        self.links().pack_url(pack)
    }

    /// A handle that issues links (for the mock API).
    pub fn links(&self) -> Links {
        Links {
            base: self.base_url(),
            state: Some(Arc::clone(&self.state)),
        }
    }

    pub fn replace_packs(&self, manifest: Vec<u8>, packs: Vec<Arc<Vec<u8>>>) {
        *self.state.manifest.lock().unwrap() = manifest;
        *self.state.packs.lock().unwrap() = packs;
    }

    /// Queues a fault for the next request of `pack` (any pack if `None`).
    pub fn inject(&self, pack: Option<u32>, fault: Fault) {
        self.state.faults.lock().unwrap().push_back((pack, fault));
    }

    pub fn pending_faults(&self) -> usize {
        self.state.faults.lock().unwrap().len()
    }

    /// Serves byte `offset` of `pack` flipped from now on.
    pub fn corrupt(&self, pack: u32, offset: u64) {
        self.state.flips.lock().unwrap().push((pack, offset));
    }

    /// Expires every link issued so far (the next ones get a new token).
    pub fn expire_links(&self) {
        let next = self.state.token.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.min_token.store(next, Ordering::SeqCst);
    }

    /// Expires every issued link once `requests` pack requests were served:
    /// later requests carrying an old link get `403` (deterministic expiry
    /// in the middle of a download).
    pub fn expire_links_after(&self, requests: u64) {
        self.state.expire_after.store(requests, Ordering::SeqCst);
    }

    pub fn set_piece_delay(&self, delay: Duration) {
        self.state
            .piece_delay_us
            .store(delay.as_micros() as u64, Ordering::Relaxed);
    }

    pub fn pack_requests(&self) -> u64 {
        self.state.pack_requests.load(Ordering::Relaxed)
    }

    /// Pack body bytes sent (the network cost of a download).
    pub fn pack_bytes(&self) -> u64 {
        self.state.pack_bytes.load(Ordering::Relaxed)
    }

    pub fn connections(&self) -> u64 {
        self.state.connections.load(Ordering::Relaxed)
    }

    /// Every served range `(pack, start, end_exclusive)`.
    pub fn ranges(&self) -> Vec<(u32, u64, u64)> {
        self.state.ranges.lock().unwrap().clone()
    }
}

/// Issues pack links with the rig's current token.
#[derive(Clone)]
pub struct Links {
    base: String,
    state: Option<Arc<State>>,
}

impl Links {
    /// Links to a rig running elsewhere (another process), with the first token.
    pub fn remote(base_url: &str) -> Self {
        Self {
            base: base_url.to_owned(),
            state: None,
        }
    }

    pub fn pack_url(&self, pack: u32) -> String {
        let token = self
            .state
            .as_ref()
            .map_or(1, |s| s.token.load(Ordering::SeqCst));
        format!("{}/packs/{pack:05}.pack?token={token}", self.base)
    }
}

struct Request {
    path: String,
    range: Option<(u64, u64)>,
}

async fn read_request(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> Option<Request> {
    let mut line = String::new();
    if reader.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let _method = parts.next()?;
    let path = parts.next()?.to_owned();
    let mut range = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await.ok()? == 0 {
            return None;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("range")
            && let Some(spec) = value.trim().strip_prefix("bytes=")
            && let Some((a, b)) = spec.split_once('-')
        {
            range = Some((a.parse().ok()?, b.parse::<u64>().ok()? + 1));
        }
    }
    Some(Request { path, range })
}

async fn serve(stream: TcpStream, state: Arc<State>) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    while let Some(request) = read_request(&mut reader).await {
        if !respond(&request, &mut write, &state).await {
            return;
        }
    }
}

async fn head(
    write: &mut tokio::net::tcp::OwnedWriteHalf,
    status: &str,
    headers: &[(String, String)],
) -> bool {
    let mut text = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text.push_str("\r\n");
    write.write_all(text.as_bytes()).await.is_ok()
}

/// Returns false when the connection must close.
async fn respond(
    request: &Request,
    write: &mut tokio::net::tcp::OwnedWriteHalf,
    state: &State,
) -> bool {
    let (path, query) = request.path.split_once('?').unwrap_or((&request.path, ""));
    if path == "/manifest.json" {
        let body = state.manifest.lock().unwrap().clone();
        let headers = vec![("Content-Length".to_owned(), body.len().to_string())];
        return head(write, "200 OK", &headers).await && write.write_all(&body).await.is_ok();
    }
    let Some(pack) = path
        .strip_prefix("/packs/")
        .and_then(|p| p.strip_suffix(".pack"))
        .and_then(|p| p.parse::<u32>().ok())
    else {
        return head(
            write,
            "404 Not Found",
            &[("Content-Length".into(), "0".into())],
        )
        .await;
    };
    let served = state.pack_requests.fetch_add(1, Ordering::SeqCst);
    let expire_after = state.expire_after.load(Ordering::SeqCst);
    if expire_after != 0 && served == expire_after {
        let next = state.token.fetch_add(1, Ordering::SeqCst) + 1;
        state.min_token.store(next, Ordering::SeqCst);
    }
    let token: u64 = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("token="))
        .and_then(|t| t.parse().ok())
        .unwrap_or(0);
    if token < state.min_token.load(Ordering::SeqCst) {
        return head(
            write,
            "403 Forbidden",
            &[("Content-Length".into(), "0".into())],
        )
        .await;
    }
    let Some(data) = state.packs.lock().unwrap().get(pack as usize).cloned() else {
        return head(
            write,
            "404 Not Found",
            &[("Content-Length".into(), "0".into())],
        )
        .await;
    };
    let size = data.len() as u64;
    let (start, end) = request.range.unwrap_or((0, size));
    if start >= end || end > size {
        return head(
            write,
            "416 Range Not Satisfiable",
            &[("Content-Length".into(), "0".into())],
        )
        .await;
    }
    let fault = {
        let mut faults = state.faults.lock().unwrap();
        let position = faults.iter().position(|(p, _)| p.is_none_or(|p| p == pack));
        position.and_then(|i| faults.remove(i)).map(|(_, f)| f)
    };
    let content_range = format!("bytes {start}-{}/{size}", end - 1);
    let mut headers = vec![(
        "Content-Type".to_owned(),
        "application/octet-stream".to_owned(),
    )];
    let (mut body_end, mut close_after) = (end, None::<u64>);
    let mut stall = None;
    match fault {
        Some(Fault::Status(code)) => {
            return head(
                write,
                &format!("{code} Injected"),
                &[("Content-Length".into(), "0".into())],
            )
            .await;
        }
        Some(Fault::IgnoreRange) => {
            headers.push(("Content-Length".into(), size.to_string()));
            return head(write, "200 OK", &headers).await
                && send(write, state, pack, &data, 0, size).await;
        }
        Some(Fault::WrongContentRange) => {
            headers.push((
                "Content-Range".into(),
                format!("bytes {}-{}/{size}", start + 1, end),
            ));
            headers.push(("Content-Length".into(), (end - start).to_string()));
        }
        Some(Fault::Truncated) => {
            body_end = start + (end - start) / 2;
            headers.push(("Content-Range".into(), content_range));
            headers.push(("Content-Length".into(), (body_end - start).to_string()));
        }
        Some(Fault::DropMidBody { after }) => {
            close_after = Some(after.min(end - start));
            headers.push(("Content-Range".into(), content_range));
            headers.push(("Content-Length".into(), (end - start).to_string()));
        }
        Some(Fault::Stall(d)) => {
            stall = Some(d);
            close_after = Some((end - start).min(64 * 1024));
            headers.push(("Content-Range".into(), content_range));
            headers.push(("Content-Length".into(), (end - start).to_string()));
        }
        None => {
            headers.push(("Content-Range".into(), content_range));
            headers.push(("Content-Length".into(), (end - start).to_string()));
        }
    }
    state.ranges.lock().unwrap().push((pack, start, end));
    if !head(write, "206 Partial Content", &headers).await {
        return false;
    }
    if let Some(n) = close_after {
        let _ = send(write, state, pack, &data, start, start + n).await;
        if let Some(d) = stall {
            tokio::time::sleep(d).await;
        }
        let _ = write.shutdown().await;
        return false;
    }
    send(write, state, pack, &data, start, body_end).await
}

async fn send(
    write: &mut tokio::net::tcp::OwnedWriteHalf,
    state: &State,
    pack: u32,
    data: &[u8],
    start: u64,
    end: u64,
) -> bool {
    const PIECE: u64 = 1024 * 1024;
    let flips: Vec<u64> = state
        .flips
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, o)| *p == pack && (start..end).contains(o))
        .map(|(_, o)| *o)
        .collect();
    let delay = Duration::from_micros(state.piece_delay_us.load(Ordering::Relaxed));
    let mut at = start;
    while at < end {
        let stop = (at + PIECE).min(end);
        let slice = &data[at as usize..stop as usize];
        let ok = if flips.iter().any(|o| (at..stop).contains(o)) {
            let mut copy = slice.to_vec();
            for o in &flips {
                if (at..stop).contains(o) {
                    copy[(o - at) as usize] ^= 0x01;
                }
            }
            write.write_all(&copy).await.is_ok()
        } else {
            write.write_all(slice).await.is_ok()
        };
        if !ok {
            return false;
        }
        state.pack_bytes.fetch_add(stop - at, Ordering::Relaxed);
        at = stop;
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
    }
    true
}
