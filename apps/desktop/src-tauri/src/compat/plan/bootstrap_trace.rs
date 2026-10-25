//! Test-only Wine loader/exception counts. Raw stderr is drained, never logged or saved.
use std::collections::BTreeMap;
use std::sync::Mutex;

use tokio::io::AsyncReadExt;

const MAX_LINE: usize = 8192;
const CODES: [(u32, &str); 7] = [
    (0xc0000005, "access_violation"),
    (0xc000001d, "illegal_instruction"),
    (0xc000007b, "invalid_image"),
    (0xc0000135, "dll_not_found"),
    (0x80000003, "breakpoint"),
    (0xe06d7363, "cpp_exception"),
    (0x406d1388, "thread_name"),
];

#[derive(Default)]
struct Trace {
    line: Vec<u8>,
    discarding: bool,
    bytes: u64,
    oversized_lines: u64,
    codes: [u64; 7],
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
            if line.contains("err:module:import_dll") {
                self.import_errors = self.import_errors.saturating_add(1);
            }
            if line.contains("err:module:loader_init")
                || line.contains("err:module:LdrInitializeThunk")
            {
                self.loader_errors = self.loader_errors.saturating_add(1);
            }
            if line.contains(":seh:")
                && let Some((_, raw)) = line.split_once("code=")
            {
                let raw = raw.strip_prefix("0x").unwrap_or(raw);
                if let Some(code) = raw
                    .get(..8)
                    .and_then(|code| u32::from_str_radix(code, 16).ok())
                    && raw
                        .as_bytes()
                        .get(8)
                        .is_none_or(|next| !next.is_ascii_hexdigit())
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
        serde_json::json!({
            "bytes": self.bytes,
            "oversized_lines": self.oversized_lines,
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
        .env("WINEDEBUG", "+seh,+module")
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
    // Both futures and the pipe belong to this call. Cancellation detaches no reader task;
    // the command's existing kill_on_drop ends the child, and Receipt emits fixed counts.
    tokio::try_join!(child.wait(), drain).map(|(status, ())| status)
}

#[test]
fn only_fixed_classes_and_counts_leave_the_trace_boundary() {
    let mut trace = Trace::default();
    let input = b"0030:trace:seh:dispatch_exception code=c0000135 flags=0 address=private-value\n0031:err:module:import_dll Library private-path needed by private-argument\n0032:err:module:LdrInitializeThunk private-module\n0033:trace:seh:dispatch_exception code=12345678 private-value\n";
    for byte in input {
        trace.feed(&[*byte]);
    }
    let report = trace.report();
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
