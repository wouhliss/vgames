//! Client IP resolution (direct peer, or `X-Forwarded-For` behind a trusted proxy).

use std::net::{IpAddr, SocketAddr};

use axum::{extract::ConnectInfo, http::request::Parts};

/// Returns the client IP. With `trust_proxy_headers`, the left-most valid `X-Forwarded-For`
/// entry wins; otherwise the TCP peer address.
pub fn client_ip(parts: &Parts, trust_proxy_headers: bool) -> Option<IpAddr> {
    if trust_proxy_headers
        && let Some(ip) = parts
            .headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
    {
        return Some(ip);
    }
    parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
}
