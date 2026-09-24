//! Logging setup with secret redaction.
//!
//! Two defences keep secrets out of logs:
//! 1. Secret values live in [`crate::secret::Secret`], whose `Debug` prints `[redacted]`.
//! 2. This formatter replaces the value of any field whose *name* looks secret
//!    (`token`, `code`, `state`, `ticket`, `secret`, `password`, `cookie`,
//!    `authorization`, `signature`, `url`, …) with `[redacted]`, in both pretty and JSON output.
//!
//! HTTP spans record the request path only, never the query string (which carries OAuth
//! codes, realtime tickets and signed storage tokens); see `http::trace_span`.

use std::fmt::{self, Write as _};

use serde_json::{Map, Value};
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{
    EnvFilter,
    fmt::{FmtContext, FormatEvent, FormatFields, FormattedFields, format::Writer},
    layer::SubscriberExt,
    registry::LookupSpan,
    util::SubscriberInitExt,
};

use crate::config::LogFormat;

const REDACTED: &str = "[redacted]";

/// Field names whose values are never written to logs.
pub fn is_sensitive_field(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    const EXACT: &[&str] = &[
        "token",
        "code",
        "state",
        "ticket",
        "secret",
        "password",
        "cookie",
        "set_cookie",
        "authorization",
        "signature",
        "url",
        "signed_url",
        "location",
        "code_verifier",
        "client_secret",
        "csrf",
        "key",
        "private_key",
    ];
    EXACT.contains(&n.as_str())
        || n.ends_with("_token")
        || n.ends_with("_secret")
        || n.ends_with("_code")
        || n.ends_with("_url")
        || n.ends_with("_key")
        || n.ends_with("_ticket")
}

/// Installs the global subscriber. Safe to call once per process.
pub fn init(filter: &str, format: LogFormat) -> Result<(), String> {
    let filter = EnvFilter::try_new(filter).map_err(|e| format!("VGAMES_LOG is invalid: {e}"))?;
    let fmt_layer = tracing_subscriber::fmt::layer()
        .fmt_fields(RedactingFields)
        .event_format(RedactingFormat {
            json: format == LogFormat::Json,
        });
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .try_init()
        .map_err(|e| e.to_string())
}

/// Collects event/span fields, redacting sensitive ones.
#[derive(Default)]
struct Collector {
    message: Option<String>,
    fields: Vec<(&'static str, Value)>,
}

impl Collector {
    fn push(&mut self, field: &Field, value: Value) {
        let name = field.name();
        if name == "message" {
            self.message = Some(match value {
                Value::String(s) => s,
                other => other.to_string(),
            });
        } else if is_sensitive_field(name) {
            self.fields.push((name, Value::String(REDACTED.into())));
        } else {
            self.fields.push((name, value));
        }
    }
}

impl Visit for Collector {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field, Value::String(format!("{value:?}")));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, Value::String(value.to_string()));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field, Value::from(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field, Value::from(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field, Value::from(value));
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push(field, Value::from(value));
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.push(field, Value::String(value.to_string()));
    }
}

fn write_kv(out: &mut String, fields: &[(&'static str, Value)]) {
    for (i, (k, v)) in fields.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        match v {
            Value::String(s) => {
                let _ = write!(out, "{k}={s:?}");
            }
            other => {
                let _ = write!(out, "{k}={other}");
            }
        }
    }
}

/// Formats span fields (stored once per span) with redaction.
pub struct RedactingFields;

impl<'writer> FormatFields<'writer> for RedactingFields {
    fn format_fields<R: tracing_subscriber::field::RecordFields>(
        &self,
        mut writer: Writer<'writer>,
        fields: R,
    ) -> fmt::Result {
        let mut c = Collector::default();
        fields.record(&mut c);
        let mut out = String::new();
        if let Some(m) = c.message {
            out.push_str(&m);
            if !c.fields.is_empty() {
                out.push(' ');
            }
        }
        write_kv(&mut out, &c.fields);
        writer.write_str(&out)
    }
}

/// Event formatter for pretty and JSON output.
pub struct RedactingFormat {
    json: bool,
}

impl<S, N> FormatEvent<S, N> for RedactingFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut c = Collector::default();
        event.record(&mut c);
        let meta = event.metadata();
        let ts = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();

        let mut spans: Vec<(String, String)> = Vec::new();
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                let ext = span.extensions();
                let fields = ext
                    .get::<FormattedFields<N>>()
                    .map(|f| f.fields.as_str().to_string())
                    .unwrap_or_default();
                spans.push((span.name().to_string(), fields));
            }
        }

        if self.json {
            let mut obj = Map::new();
            obj.insert("timestamp".into(), Value::String(ts));
            obj.insert("level".into(), Value::String(meta.level().to_string()));
            obj.insert("target".into(), Value::String(meta.target().to_string()));
            if let Some(m) = c.message {
                obj.insert("message".into(), Value::String(m));
            }
            let mut fields = Map::new();
            for (k, v) in c.fields {
                fields.insert(k.to_string(), v);
            }
            if !fields.is_empty() {
                obj.insert("fields".into(), Value::Object(fields));
            }
            if !spans.is_empty() {
                let list = spans
                    .into_iter()
                    .map(|(name, fields)| {
                        let mut s = Map::new();
                        s.insert("name".into(), Value::String(name));
                        if !fields.is_empty() {
                            s.insert("fields".into(), Value::String(fields));
                        }
                        Value::Object(s)
                    })
                    .collect();
                obj.insert("spans".into(), Value::Array(list));
            }
            writeln!(writer, "{}", Value::Object(obj))
        } else {
            let mut out = format!("{ts} {:>5} ", meta.level());
            for (name, fields) in &spans {
                if fields.is_empty() {
                    let _ = write!(out, "{name}:");
                } else {
                    let _ = write!(out, "{name}{{{fields}}}:");
                }
            }
            let _ = write!(out, " {}: ", meta.target());
            if let Some(m) = &c.message {
                out.push_str(m);
                if !c.fields.is_empty() {
                    out.push(' ');
                }
            }
            write_kv(&mut out, &c.fields);
            writeln!(writer, "{out}")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    use super::*;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| std::io::Error::other("poisoned"))?
                .extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Buf {
        type Writer = Buf;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn capture(json: bool, f: impl FnOnce()) -> String {
        let buf = Buf::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_writer(buf.clone())
                .fmt_fields(RedactingFields)
                .event_format(RedactingFormat { json }),
        );
        tracing::subscriber::with_default(subscriber, f);
        let bytes = buf.0.lock().map(|b| b.clone()).unwrap_or_default();
        String::from_utf8(bytes).unwrap_or_default()
    }

    #[test]
    fn sensitive_fields_are_redacted_in_both_formats() {
        for json in [false, true] {
            let out = capture(json, || {
                let span = tracing::info_span!("req", path = "/v1/x", ticket = "tkt-123");
                let _g = span.enter();
                tracing::info!(
                    access_token = "vga_SECRET",
                    user_id = 7,
                    signed_url = "https://x?sig=abc",
                    "hello"
                );
            });
            assert!(out.contains("hello"), "{out}");
            assert!(out.contains("user_id"), "{out}");
            assert!(out.contains("/v1/x"), "{out}");
            for leaked in ["vga_SECRET", "sig=abc", "tkt-123"] {
                assert!(!out.contains(leaked), "leaked {leaked} in {out}");
            }
            assert!(out.contains(REDACTED), "{out}");
        }
    }

    #[test]
    fn classifies_field_names() {
        for n in [
            "token",
            "refresh_token",
            "code",
            "login_code",
            "state",
            "client_secret",
            "Authorization",
            "manifest_url",
        ] {
            assert!(is_sensitive_field(n), "{n}");
        }
        for n in ["user_id", "path", "status", "package_id", "message"] {
            assert!(!is_sensitive_field(n), "{n}");
        }
    }
}
