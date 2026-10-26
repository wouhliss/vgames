//! Test-only Wine loader/exception counts. Raw stderr is drained, never logged or saved.
use std::collections::BTreeMap;
use std::sync::Mutex;

use tokio::io::AsyncReadExt;

const MAX_LINE: usize = 8192;
const CODES: [(u32, &str); 25] = [
    (0xc0000005, "access_violation"),
    (0xc000001d, "illegal_instruction"),
    (0xc000007b, "invalid_image"),
    (0xc0000135, "dll_not_found"),
    (0x80000003, "breakpoint"),
    (0xe06d7363, "cpp_exception"),
    (0x406d1388, "thread_name"),
    (0x40010006, "debug_print_ansi"),
    (0x4001000a, "debug_print_unicode"),
    (0x80000100, "wine_stub"),
    (0x80000101, "wine_assertion"),
    (0x000006ba, "rpc_server_unavailable"),
    (0x000006be, "rpc_call_failed"),
    (0x000006d9, "rpc_no_endpoints"),
    (0x000006f7, "rpc_bad_stub_data"),
    (0xc0000142, "dll_init_failed"),
    (0xc0000139, "entry_point_not_found"),
    (0xc00000fd, "stack_overflow"),
    (0xc0000008, "invalid_handle"),
    (0xc0000006, "in_page_error"),
    (0xc0000194, "possible_deadlock"),
    (0xc0000374, "heap_corruption"),
    (0xc000041d, "callback_exception"),
    (0xc0000420, "assertion_failure"),
    (0xe0434352, "clr_exception"),
];

const PHASES: [(&str, &str, &str); 7] = [
    ("appwizcpl", "get_cache_file_name", "addon_cache_events"),
    ("appwizcpl", "get_url", "addon_url_events"),
    (
        "appwizcpl",
        "InstallCallback_GetBindInfo",
        "addon_download_events",
    ),
    ("appwizcpl", "install_file", "addon_install_errors"),
    ("msi", "MsiInstallProductW", "msi_install_events"),
    ("dialog", "DIALOG_CreateIndirect", "dialog_create_events"),
    (
        "dialog",
        "DialogBoxIndirectParamAorW",
        "modal_dialog_events",
    ),
];
const ERRORS: [&str; 5] = ["winediag", "winemac", "rpc", "ole", "service"];

// Function tags are parsed only from the Wine header, never searched for in payload text.
fn header(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.splitn(4, ':');
    let thread = parts.next()?;
    if thread.is_empty() || thread.len() > 8 || !thread.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let level = parts.next()?;
    if !matches!(level, "trace" | "warn" | "err" | "fixme") {
        return None;
    }
    Some((
        level,
        parts.next()?,
        parts.next()?.split_ascii_whitespace().next()?,
    ))
}

#[derive(Default)]
struct Trace {
    line: Vec<u8>,
    discarding: bool,
    bytes: u64,
    oversized_lines: u64,
    parsed_headers: u64,
    codes: [u64; CODES.len()],
    phases: [u64; PHASES.len()],
    errors: [u64; ERRORS.len()],
    other_exceptions: u64,
    import_errors: u64,
    loader_errors: u64,
}

impl Trace {
    fn feed(&mut self, bytes: &[u8]) {
        self.bytes = self.bytes.saturating_add(bytes.len() as u64);
        for &byte in bytes {
            if byte == b'\n' {
                self.finish();
            } else if !self.discarding {
                if self.line.len() == MAX_LINE {
                    self.line.clear();
                    self.discarding = true;
                    self.oversized_lines = self.oversized_lines.saturating_add(1);
                } else {
                    self.line.push(byte);
                }
            }
        }
    }

    fn finish(&mut self) {
        if !self.discarding {
            let line = String::from_utf8_lossy(&self.line);
            let fields = header(&line);
            if let Some((level, channel, function)) = fields {
                self.parsed_headers = self.parsed_headers.saturating_add(1);
                if let Some(index) = PHASES
                    .iter()
                    .position(|(known_channel, known_function, _)| {
                        *known_channel == channel && *known_function == function
                    })
                    && let Some(count) = self.phases.get_mut(index)
                {
                    *count = count.saturating_add(1);
                }
                if level == "err" {
                    if let Some(index) = ERRORS.iter().position(|known| *known == channel)
                        && let Some(count) = self.errors.get_mut(index)
                    {
                        *count = count.saturating_add(1);
                    }
                    if channel == "module" && function == "import_dll" {
                        self.import_errors = self.import_errors.saturating_add(1);
                    }
                    if channel == "module"
                        && matches!(function, "loader_init" | "LdrInitializeThunk")
                    {
                        self.loader_errors = self.loader_errors.saturating_add(1);
                    }
                }
            }
            if fields.is_some_and(|(_, channel, _)| channel == "seh")
                && let Some((_, raw)) = line.split_once("code=")
            {
                let raw = raw.strip_prefix("0x").unwrap_or(raw);
                let digits = raw.bytes().take_while(u8::is_ascii_hexdigit).count();
                if (1..=8).contains(&digits)
                    && let Some(code) = raw
                        .get(..digits)
                        .and_then(|code| u32::from_str_radix(code, 16).ok())
                {
                    if let Some(index) = CODES.iter().position(|(known, _)| *known == code) {
                        if let Some(count) = self.codes.get_mut(index) {
                            *count = count.saturating_add(1);
                        }
                    } else {
                        self.other_exceptions = self.other_exceptions.saturating_add(1);
                    }
                }
            }
        }
        self.line.clear();
        self.discarding = false;
    }

    fn report(&self) -> serde_json::Value {
        let codes: BTreeMap<_, _> = CODES
            .iter()
            .zip(self.codes)
            .map(|((_, name), count)| (*name, count))
            .collect();
        let phases: BTreeMap<_, _> = PHASES
            .iter()
            .zip(self.phases)
            .map(|((_, _, name), count)| (*name, count))
            .collect();
        let errors: BTreeMap<_, _> = ERRORS
            .iter()
            .zip(self.errors)
            .map(|(name, count)| (*name, count))
            .collect();
        serde_json::json!({
            "phase_events": phases,
            "error_channels": errors,
            "bytes": self.bytes,
            "oversized_lines": self.oversized_lines,
            "parsed_headers": self.parsed_headers,
            "exception_classes": codes,
            "other_exceptions": self.other_exceptions,
            "import_errors": self.import_errors,
            "loader_errors": self.loader_errors,
        })
    }
}

struct Receipt(Mutex<Trace>);
impl Drop for Receipt {
    fn drop(&mut self) {
        // Also runs when the existing five-minute timeout drops the whole future.
        let report = match self.0.lock() {
            Ok(mut trace) => {
                trace.finish();
                trace.report()
            }
            Err(_) => serde_json::json!({ "available": false }),
        };
        eprintln!("Wine bootstrap trace: {report}");
    }
}

pub(super) async fn status(
    command: &mut tokio::process::Command,
    bootstrap: bool,
) -> std::io::Result<std::process::ExitStatus> {
    if !bootstrap {
        return command.status().await;
    }
    command
        .env("WINEDEBUG", "+seh,+module,+appwizcpl,+dialog,+msi,err+winediag,err+winemac,err+rpc,err+ole,err+service")
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn()?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("diagnostic pipe unavailable"))?;
    let receipt = Receipt(Mutex::new(Trace::default()));
    let drain = async {
        let mut chunk = [0; 4096];
        loop {
            let count = stderr.read(&mut chunk).await?;
            if count == 0 {
                return Ok::<_, std::io::Error>(());
            }
            if let Ok(mut trace) = receipt.0.lock() {
                trace.feed(&chunk[..count]);
            }
            tokio::task::yield_now().await;
        }
    };
    // The parent may finish while a descendant still owns stderr. Preserve status()
    // completion semantics: cancel our reader on parent exit instead of awaiting pipe EOF.
    // Both futures remain owned here, including when the existing timeout cancels this call.
    tokio::select! {
        status = child.wait() => status,
        drained = drain => {
            drained?;
            child.wait().await
        }
    }
}

#[test]
fn only_fixed_classes_and_counts_leave_the_trace_boundary() {
    let mut trace = Trace::default();
    let input = b"0030:trace:seh:dispatch_exception code=c0000135 flags=0 address=private-value\n0031:err:module:import_dll Library private-path needed by private-argument\n0032:err:module:LdrInitializeThunk private-module\n0033:trace:seh:dispatch_exception code=12345678 private-value\n";
    for byte in input {
        trace.feed(&[*byte]);
    }
    trace.feed(b"0034:trace:appwizcpl:get_cache_file_name private-path\n0035:trace:dialog:DIALOG_CreateIndirect private-title\n0036:trace:module:load_dll payload appwizcpl:get_cache_file_name private-value\n0037:trace:seh:dispatch_exception code=6ba private-value\n0038:err:winediag:nodrv_CreateWindow private-argument\n");
    let report = trace.report();
    assert_eq!(report["phase_events"]["addon_cache_events"], 1);
    assert_eq!(report["phase_events"]["dialog_create_events"], 1);
    assert_eq!(report["exception_classes"]["rpc_server_unavailable"], 1);
    assert_eq!(report["error_channels"]["winediag"], 1);
    assert_eq!(report["exception_classes"]["dll_not_found"], 1);
    assert_eq!(report["import_errors"], 1);
    assert_eq!(report["loader_errors"], 1);
    assert_eq!(report["other_exceptions"], 1);
    let output = report.to_string();
    for value in ["private", "12345678", "c0000135", "address", "Library"] {
        assert!(!output.contains(value));
    }
}

#[test]
fn oversized_lines_are_discarded_without_hiding_later_complete_events() {
    let mut trace = Trace::default();
    for _ in 0..100 {
        trace.feed(&[b'x'; 4096]);
        assert!(trace.line.len() <= MAX_LINE);
    }
    trace.feed(b"code=c0000135\n0030:trace:seh:dispatch_exception code=80000003 flags=0");
    trace.finish();
    assert_eq!(trace.report()["oversized_lines"], 1);
    assert_eq!(trace.report()["exception_classes"]["dll_not_found"], 0);
    assert_eq!(trace.report()["exception_classes"]["breakpoint"], 1);
}

#[cfg(unix)]
#[tokio::test]
async fn completed_bootstrap_does_not_wait_for_a_descendants_stderr() {
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .args(["-c", "/bin/sleep 3 & exit 0"])
        .env_clear()
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null());
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        status(&mut command, true),
    )
    .await;
    assert!(result.is_ok_and(|result| result.is_ok_and(|status| status.success())));
}
