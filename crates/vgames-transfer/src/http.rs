//! HTTP clients for bulk transfers to and from object storage.
//!
//! Transfers use **HTTP/1.1 only**: one TCP connection (and congestion window)
//! per concurrent request is what saturates a fast link (02 §7.6); HTTP/2
//! would multiplex every range over one connection. Bodies are never
//! content-decoded (a transparent gzip would break byte ranges). Signed URLs
//! carry their own credentials, so no auth header is ever attached here.

use std::time::Duration;

/// Idle connections kept per host: one per worker at the concurrency ceiling.
pub const MAX_CONNECTIONS: usize = 32;

/// Options for [`transfer_client`].
#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub connect_timeout: Duration,
    /// Keep idle connections for reuse (`false`: every request opens a new
    /// connection, used to retry a corrupted chunk on a fresh one).
    pub reuse_connections: bool,
    /// Honor the system proxy settings (off in tests against loopback rigs).
    pub use_system_proxy: bool,
    pub user_agent: String,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            reuse_connections: true,
            use_system_proxy: true,
            user_agent: format!("vgames-transfer/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// Builds a client for range downloads and resumable uploads.
pub fn transfer_client(options: &ClientOptions) -> reqwest::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .http1_only()
        .no_gzip()
        .tcp_nodelay(true)
        .connect_timeout(options.connect_timeout)
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(if options.reuse_connections {
            MAX_CONNECTIONS
        } else {
            0
        })
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(options.user_agent.clone());
    if !options.use_system_proxy {
        builder = builder.no_proxy();
    }
    builder.build()
}

/// A URL safe to log: scheme, host and path only. Signed URLs carry their
/// credentials in the query string, which is never logged (00-overview §8).
pub fn redact_url(url: &str) -> String {
    let without_query = url.split(['?', '#']).next().unwrap_or_default();
    if without_query.len() < url.len() {
        format!("{without_query}?…")
    } else {
        without_query.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_strings_are_redacted() {
        assert_eq!(
            redact_url("https://storage.example/v1/p/v/packs/00001.pack?X-Goog-Signature=abc"),
            "https://storage.example/v1/p/v/packs/00001.pack?…"
        );
        assert_eq!(redact_url("https://a.example/x"), "https://a.example/x");
    }

    #[test]
    fn clients_build() {
        transfer_client(&ClientOptions::default()).unwrap();
        transfer_client(&ClientOptions {
            reuse_connections: false,
            use_system_proxy: false,
            ..ClientOptions::default()
        })
        .unwrap();
    }
}
