//! Logging: a size-rotated file in the app log dir (7 files × 10 MB), written
//! from a dedicated thread, with secrets scrubbed from every line, plus a
//! panic hook that writes a local crash report (never uploaded).
//!
//! Redaction is a safety net. Code must still never log tokens, codes, keys
//! or signed URLs on purpose (00-overview §8).

use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Environment variable with an `EnvFilter` directive (for example `debug`).
pub const LOG_ENV: &str = "VGAMES_LOG";
const LOG_FILE: &str = "vgames.log";
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// The live file plus six rotated ones.
const MAX_FILES: usize = 7;
const MAX_CRASH_REPORTS: usize = 20;

static CRASH_DIR: OnceLock<PathBuf> = OnceLock::new();

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("cannot open the log file in {dir}")]
    Open {
        dir: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("a global logger is already installed")]
    AlreadyInstalled,
}

/// Keeps the background log writer alive; dropping it flushes pending lines.
pub struct LogGuard {
    _worker: tracing_appender::non_blocking::WorkerGuard,
}

/// Installs the global subscriber. Call once, as early as the log dir is known.
pub fn init(log_dir: &Path) -> Result<LogGuard, LoggingError> {
    let file =
        RollingFile::open(log_dir, LOG_FILE, MAX_FILE_BYTES, MAX_FILES).map_err(|source| {
            LoggingError::Open {
                dir: log_dir.to_owned(),
                source,
            }
        })?;
    // The worker thread blocks on its channel: no wakeups while nothing is logged.
    let (writer, worker) = tracing_appender::non_blocking(RedactingWriter::new(file));

    let default_directive = if cfg!(debug_assertions) {
        "info,vgames_desktop_lib=debug,vgames_transfer=debug"
    } else {
        "info"
    };
    let filter =
        EnvFilter::try_from_env(LOG_ENV).unwrap_or_else(|_| EnvFilter::new(default_directive));

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_writer(writer);
    let stderr_layer = cfg!(debug_assertions).then(|| {
        tracing_subscriber::fmt::layer()
            .with_target(true)
            .with_writer(|| RedactingWriter::new(io::stderr()))
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .map_err(|_| LoggingError::AlreadyInstalled)?;

    Ok(LogGuard { _worker: worker })
}

// ---------------------------------------------------------------------------
// Size-based rotation

/// `vgames.log` rotated to `vgames.log.1` … `vgames.log.{max_files-1}` when it
/// would grow past `max_bytes`.
struct RollingFile {
    dir: PathBuf,
    name: String,
    max_bytes: u64,
    max_files: usize,
    file: File,
    written: u64,
}

impl RollingFile {
    fn open(dir: &Path, name: &str, max_bytes: u64, max_files: usize) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(name);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            dir: dir.to_owned(),
            name: name.to_owned(),
            max_bytes,
            max_files: max_files.max(1),
            file,
            written,
        })
    }

    fn rotated(&self, index: usize) -> PathBuf {
        self.dir.join(format!("{}.{index}", self.name))
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let oldest = self.max_files - 1;
        if oldest == 0 {
            self.file = File::create(self.dir.join(&self.name))?;
            self.written = 0;
            return Ok(());
        }
        remove_if_exists(&self.rotated(oldest))?;
        for index in (1..oldest).rev() {
            rename_if_exists(&self.rotated(index), &self.rotated(index + 1))?;
        }
        rename_if_exists(&self.dir.join(&self.name), &self.rotated(1))?;
        self.file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(self.dir.join(&self.name))?;
        self.written = 0;
        Ok(())
    }
}

impl Write for RollingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len() as u64;
        if self.written > 0 && self.written.saturating_add(len) > self.max_bytes {
            self.rotate()?;
        }
        let n = self.file.write(buf)?;
        self.written = self.written.saturating_add(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn rename_if_exists(from: &Path, to: &Path) -> io::Result<()> {
    // Windows refuses to rename over an existing file.
    remove_if_exists(to)?;
    match std::fs::rename(from, to) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Redaction

/// Scrubs secrets from each chunk written. `tracing`'s formatter writes one
/// whole event per call, so patterns are never split across calls.
struct RedactingWriter<W> {
    inner: W,
}

impl<W: Write> RedactingWriter<W> {
    fn new(inner: W) -> Self {
        Self { inner }
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        self.inner.write_all(redact(&text).as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

const REDACTED: &str = "[redacted]";

/// Keys whose values are secrets, matched case-insensitively as whole words
/// followed by `=`, `: ` or `":`.
const SECRET_KEYS: &[&str] = &[
    "access_token",
    "refresh_token",
    "id_token",
    "token",
    "login_code",
    "auth_code",
    "code_verifier",
    "client_state",
    "oauth_state",
    "client_secret",
    "password",
    "passphrase",
    "secret",
    "join_secret",
    "private_key",
    "signature",
    "x-goog-signature",
    "cookie",
    "set-cookie",
    "authorization",
    "ticket",
];

const TOKEN_PREFIXES: &[&str] = &["vga_", "vgr_", "vgs_"];

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn is_base64url(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn ends_url_query(b: u8) -> bool {
    b.is_ascii_whitespace() || matches!(b, b'"' | b'\'' | b'`' | b'<' | b'>' | b')' | b']' | b'}')
}

fn ends_bare_value(b: u8) -> bool {
    b.is_ascii_whitespace() || matches!(b, b'&' | b',' | b';' | b'}' | b')' | b']' | b'"' | b'\'')
}

fn starts_with_ignore_case(haystack: &[u8], needle: &str) -> bool {
    haystack.len() >= needle.len()
        && haystack
            .iter()
            .zip(needle.as_bytes())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// Replaces tokens, bearer credentials, URL query strings and `key=value`
/// secrets with `[redacted]`. Only ASCII delimiters are used as cut points, so
/// the output stays valid UTF-8.
pub fn redact(input: &str) -> Cow<'_, str> {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut changed = false;
    let mut copied_to = 0; // bytes[..copied_to] are already in `out`
    let mut i = 0;
    let mut in_url = false;

    macro_rules! replace {
        ($keep_until:expr, $skip_to:expr) => {{
            out.extend_from_slice(bytes.get(copied_to..$keep_until).unwrap_or_default());
            out.extend_from_slice(REDACTED.as_bytes());
            copied_to = $skip_to;
            i = $skip_to;
            changed = true;
            continue;
        }};
    }

    while i < bytes.len() {
        let rest = bytes.get(i..).unwrap_or_default();
        let Some(&b) = rest.first() else { break };
        let at_boundary = i == 0 || bytes.get(i - 1).is_none_or(|&p| !is_word_byte(p));

        if b.is_ascii_whitespace() || matches!(b, b'"' | b'\'' | b'<' | b'>') {
            in_url = false;
        }
        if rest.starts_with(b"://") {
            in_url = true;
        }

        // 1. Our own token formats (vga_/vgr_/vgs_ + base64url).
        if at_boundary
            && let Some(prefix) = TOKEN_PREFIXES
                .iter()
                .find(|p| rest.starts_with(p.as_bytes()))
        {
            let body = rest.get(prefix.len()..).unwrap_or_default();
            let n = body.iter().take_while(|&&c| is_base64url(c)).count();
            if n >= 8 {
                let keep = i + prefix.len();
                replace!(keep, keep + n);
            }
        }

        // 2. Bearer credentials.
        if at_boundary && starts_with_ignore_case(rest, "bearer ") {
            let start = i + "bearer ".len();
            let n = bytes
                .get(start..)
                .unwrap_or_default()
                .iter()
                .take_while(|&&c| !c.is_ascii_whitespace() && !matches!(c, b'"' | b'\'' | b','))
                .count();
            if n > 0 {
                replace!(start, start + n);
            }
        }

        // 3. Query strings of URLs (signed URLs, OAuth callbacks).
        if in_url && b == b'?' {
            let start = i + 1;
            let n = bytes
                .get(start..)
                .unwrap_or_default()
                .iter()
                .take_while(|&&c| !ends_url_query(c))
                .count();
            if n > 0 {
                replace!(start, start + n);
            }
        }

        // 4. key=value, key: value, "key":"value".
        if at_boundary
            && let Some(key) = SECRET_KEYS.iter().find(|k| {
                starts_with_ignore_case(rest, k)
                    && rest.get(k.len()).is_none_or(|&next| !is_word_byte(next))
            })
        {
            let after_key = i + key.len();
            let tail = bytes.get(after_key..).unwrap_or_default();
            let separator = [&b"\":"[..], b"\": ", b"=", b": "]
                .iter()
                .filter(|sep| tail.starts_with(sep))
                .map(|sep| sep.len())
                .max();
            if let Some(sep_len) = separator {
                let mut value_start = after_key + sep_len;
                // Skip optional spaces after `":`.
                while bytes.get(value_start) == Some(&b' ') {
                    value_start += 1;
                }
                let value = bytes.get(value_start..).unwrap_or_default();
                if value.first() == Some(&b'"') {
                    // Quoted: redact up to the closing quote, honoring escapes.
                    let mut j = 1;
                    while let Some(&c) = value.get(j) {
                        if c == b'\\' {
                            j += 2;
                            continue;
                        }
                        if c == b'"' {
                            break;
                        }
                        j += 1;
                    }
                    let end = (value_start + j).min(bytes.len());
                    if end > value_start + 1 {
                        replace!(value_start + 1, end);
                    }
                } else {
                    // Keep an HTTP auth scheme visible: `Bearer [redacted]`.
                    let scheme = ["bearer ", "basic "]
                        .iter()
                        .find(|s| starts_with_ignore_case(value, s))
                        .map_or(0, |s| s.len());
                    let value_start = value_start + scheme;
                    let value = bytes.get(value_start..).unwrap_or_default();
                    let n = value.iter().take_while(|&&c| !ends_bare_value(c)).count();
                    if n > 0 {
                        replace!(value_start, value_start + n);
                    }
                }
            }
        }

        i += 1;
    }

    if !changed {
        return Cow::Borrowed(input);
    }
    out.extend_from_slice(bytes.get(copied_to..).unwrap_or_default());
    match String::from_utf8(out) {
        Ok(s) => Cow::Owned(s),
        Err(e) => Cow::Owned(String::from_utf8_lossy(e.as_bytes()).into_owned()),
    }
}

// ---------------------------------------------------------------------------
// Crash reports

/// Installs a panic hook that writes `crash-<time>-<pid>.txt` into the log dir
/// (once [`set_crash_dir`] has run) and then calls the previous hook.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = panic_message(info);
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_owned());
        tracing::error!(%location, message = %redact(&message), "panic");
        if let Some(dir) = CRASH_DIR.get() {
            let _ = write_crash_report(dir, &message, &location);
        }
        previous(info);
    }));
}

/// Where crash reports go. The first call wins.
pub fn set_crash_dir(dir: PathBuf) {
    let _ = CRASH_DIR.set(dir);
}

fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

fn write_crash_report(dir: &Path, message: &str, location: &str) -> io::Result<PathBuf> {
    let now = time::OffsetDateTime::now_utc();
    let stamp = format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    );
    let path = dir.join(format!("crash-{stamp}-{}.txt", std::process::id()));
    let thread = std::thread::current();
    let backtrace = std::backtrace::Backtrace::force_capture();
    let report = format!(
        "vgames crash report\n\
         version: {version}\n\
         os: {os} {arch}\n\
         time: {stamp}\n\
         thread: {thread}\n\
         location: {location}\n\
         message: {message}\n\n\
         backtrace:\n{backtrace}\n",
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        thread = thread.name().unwrap_or("unnamed"),
        message = redact(message),
    );
    std::fs::write(&path, redact(&report).as_bytes())?;
    prune_crash_reports(dir);
    Ok(path)
}

fn prune_crash_reports(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut reports: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("crash-") && n.ends_with(".txt"))
        })
        .collect();
    if reports.len() <= MAX_CRASH_REPORTS {
        return;
    }
    // Names embed a sortable UTC timestamp.
    reports.sort();
    let excess = reports.len() - MAX_CRASH_REPORTS;
    for old in reports.iter().take(excess) {
        let _ = std::fs::remove_file(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_our_tokens() {
        let line = "sending vga_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_abcdefg and vgr_zzzzzzzzzzzz";
        assert_eq!(redact(line), "sending vga_[redacted] and vgr_[redacted]");
        // Too short to be a token, or inside a word: untouched.
        assert_eq!(redact("vga_x"), "vga_x");
        assert_eq!(redact("xvga_abcdefghijk"), "xvga_abcdefghijk");
    }

    #[test]
    fn redacts_bearer_and_authorization() {
        assert_eq!(
            redact("Authorization: Bearer abc.def"),
            "Authorization: Bearer [redacted]"
        );
        assert_eq!(
            redact("header bearer xyz, next"),
            "header bearer [redacted], next"
        );
    }

    #[test]
    fn redacts_url_queries() {
        let url =
            "GET https://storage.googleapis.com/b/o?X-Goog-Signature=abc&X-Goog-Date=1 failed";
        assert_eq!(
            redact(url),
            "GET https://storage.googleapis.com/b/o?[redacted] failed"
        );
        let deep = "open \"vgames://auth/callback?code=secret&client_state=s\"";
        assert_eq!(redact(deep), "open \"vgames://auth/callback?[redacted]\"");
        // A question mark outside a URL is kept.
        assert_eq!(redact("really? yes"), "really? yes");
    }

    #[test]
    fn redacts_key_values() {
        assert_eq!(
            redact("refresh_token=abc next=1"),
            "refresh_token=[redacted] next=1"
        );
        assert_eq!(
            redact(r#"{"access_token":"a\"b","x":1}"#),
            r#"{"access_token":"[redacted]","x":1}"#
        );
        assert_eq!(
            redact(r#"{"passphrase": "hunter2"}"#),
            r#"{"passphrase": "[redacted]"}"#
        );
        assert_eq!(
            redact("Ticket { ticket: \"t1\" }"),
            "Ticket { ticket: \"[redacted]\" }"
        );
        // Longer identifiers that merely contain a key are kept.
        assert_eq!(redact("error_token_count=3"), "error_token_count=3");
        assert_eq!(redact("state=installing"), "state=installing");
    }

    #[test]
    fn keeps_unicode_intact() {
        let line = "héllo wörld token=ñandú done ✓";
        assert_eq!(redact(line), "héllo wörld token=[redacted] done ✓");
        assert!(matches!(redact("plain ✓ text"), Cow::Borrowed(_)));
    }

    #[test]
    fn rotation_keeps_at_most_max_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = RollingFile::open(dir.path(), "t.log", 100, 3).unwrap();
        for i in 0..20 {
            file.write_all(format!("{i:0>60}\n").as_bytes()).unwrap();
        }
        file.flush().unwrap();
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["t.log", "t.log.1", "t.log.2"]);
        for name in &names {
            let len = std::fs::metadata(dir.path().join(name)).unwrap().len();
            assert!(len <= 100, "{name} is {len} bytes");
        }
        // The newest line is in the live file.
        let live = std::fs::read_to_string(dir.path().join("t.log")).unwrap();
        assert!(live.contains(&format!("{:0>60}", 19)));
    }

    #[test]
    fn reopening_appends_and_counts_existing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("t.log"), vec![b'x'; 90]).unwrap();
        let mut file = RollingFile::open(dir.path(), "t.log", 100, 2).unwrap();
        file.write_all(&[b'y'; 20]).unwrap();
        assert_eq!(
            std::fs::metadata(dir.path().join("t.log.1")).unwrap().len(),
            90
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("t.log")).unwrap().len(),
            20
        );
    }

    #[test]
    fn crash_report_is_written_and_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_crash_report(dir.path(), "boom with vga_abcdefghijklmnop", "src/x.rs:1:1")
            .unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("location: src/x.rs:1:1"));
        assert!(text.contains("vga_[redacted]"));
        assert!(!text.contains("abcdefghijklmnop"));
    }

    #[test]
    fn crash_reports_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(MAX_CRASH_REPORTS + 5) {
            std::fs::write(dir.path().join(format!("crash-2026{i:04}.txt")), "x").unwrap();
        }
        prune_crash_reports(dir.path());
        let count = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(count, MAX_CRASH_REPORTS);
        assert!(!dir.path().join("crash-20260000.txt").exists());
    }
}
