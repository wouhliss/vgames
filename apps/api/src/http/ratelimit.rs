//! Rate limits (docs/architecture/01-security.md §4.4), per server instance.
//!
//! Keyed GCRA limiters from `governor`. Every request is limited (A1-T16):
//!
//! - `/v1/auth/*` per IP ([`Policy::Auth`]); signed storage URLs (`/_storage/*`, fs backend)
//!   per IP ([`Policy::Storage`]); static files (`/admin/*`, `/docs/*`) per IP
//!   ([`Policy::Static`]);
//! - routes that authenticate the caller (bearer or cookie in the OpenAPI document) per user
//!   ([`Policy::User`], applied when the session resolves), and per IP when the request
//!   carries no credentials;
//! - every other route (discovery, trust bundle, health, docs, admin UI, realtime upgrade,
//!   unknown paths) per IP ([`Policy::Public`]) whether or not it carries credentials, so a
//!   junk `Authorization` header cannot switch the limit off;
//! - failed authentication with credentials counts per IP too ([`Policy::Public`]);
//! - specific actions in their handlers with [`RateLimits::check`].

use std::{
    collections::HashSet,
    net::IpAddr,
    num::NonZeroU32,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::{
    extract::{MatchedPath, Request, State},
    http::Method,
    middleware::Next,
    response::{IntoResponse, Response},
};
use governor::{
    DefaultKeyedRateLimiter, Quota, RateLimiter,
    clock::{Clock, DefaultClock},
};

use crate::{error::ApiError, http::client_ip::client_ip, state::AppState};

/// A named limit. Quotas follow 01-security §4.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Policy {
    /// `/v1/auth/*` per IP: 20 / min.
    Auth,
    /// Any authenticated request per user: 600 / min.
    User,
    /// Friend requests and friend-code redemptions per user: 30 / h.
    FriendRequests,
    /// Invites per user: 60 / h.
    Invites,
    /// Message sends per device: 120 / min.
    Messages,
    /// Signed download URLs per user: 2,000 / h (checked with the URL count).
    DownloadUrls,
    /// Unauthenticated public endpoints per IP (discovery, trust bundle): 300 / min.
    Public,
    /// Signed storage URLs served by the API (fs backend) per IP: 6,000 / min, enough for
    /// parallel range downloads at full speed.
    Storage,
    /// Static files (admin UI, Swagger UI) per IP: 3,000 / min. No database work.
    Static,
}

impl Policy {
    pub const ALL: [Policy; 9] = [
        Policy::Auth,
        Policy::User,
        Policy::FriendRequests,
        Policy::Invites,
        Policy::Messages,
        Policy::DownloadUrls,
        Policy::Public,
        Policy::Storage,
        Policy::Static,
    ];

    pub fn quota(self) -> Quota {
        let n = |v: u32| NonZeroU32::new(v).unwrap_or(NonZeroU32::MIN);
        match self {
            Policy::Auth => Quota::per_minute(n(20)),
            Policy::User => Quota::per_minute(n(600)),
            Policy::FriendRequests => Quota::per_hour(n(30)),
            Policy::Invites => Quota::per_hour(n(60)),
            Policy::Messages => Quota::per_minute(n(120)),
            Policy::DownloadUrls => Quota::per_hour(n(2000)),
            Policy::Public => Quota::per_minute(n(300)),
            Policy::Storage => Quota::per_minute(n(6000)),
            Policy::Static => Quota::per_minute(n(3000)),
        }
    }
}

pub struct RateLimits {
    limiters: Vec<(Policy, DefaultKeyedRateLimiter<String>)>,
    checks: AtomicU64,
}

impl Default for RateLimits {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimits {
    pub fn new() -> Self {
        Self::with_quotas(Policy::quota)
    }

    /// Limits with other quotas (tests use tiny ones).
    pub fn with_quotas(quota: impl Fn(Policy) -> Quota) -> Self {
        Self {
            limiters: Policy::ALL
                .iter()
                .map(|p| (*p, RateLimiter::keyed(quota(*p))))
                .collect(),
            checks: AtomicU64::new(0),
        }
    }

    fn limiter(&self, policy: Policy) -> Option<&DefaultKeyedRateLimiter<String>> {
        self.limiters
            .iter()
            .find(|(p, _)| *p == policy)
            .map(|(_, l)| l)
    }

    /// Consumes one unit for `key`, or returns `429 rate_limited` with `Retry-After`.
    pub fn check(&self, policy: Policy, key: &str) -> Result<(), ApiError> {
        self.check_n(policy, key, 1)
    }

    /// Consumes `n` units (e.g. the number of URLs requested).
    pub fn check_n(&self, policy: Policy, key: &str, n: u32) -> Result<(), ApiError> {
        self.maybe_gc();
        let Some(limiter) = self.limiter(policy) else {
            return Ok(());
        };
        let Some(n) = NonZeroU32::new(n) else {
            return Ok(());
        };
        match limiter.check_key_n(&key.to_string(), n) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(not_until)) => {
                let wait = not_until.wait_time_from(DefaultClock::default().now());
                Err(ApiError::rate_limited(wait.as_secs().saturating_add(1)))
            }
            // More units than the burst allows: can never succeed.
            Err(_) => Err(ApiError::rate_limited(3600)),
        }
    }

    /// Drops idle keys now and then so memory stays bounded.
    fn maybe_gc(&self) {
        if self.checks.fetch_add(1, Ordering::Relaxed) % 10_000 == 9_999 {
            for (_, l) in &self.limiters {
                l.retain_recent();
                l.shrink_to_fit();
            }
        }
    }
}

/// `(METHOD, path template)` of every operation that authenticates its caller.
fn authenticated_routes() -> &'static HashSet<(String, String)> {
    static ROUTES: OnceLock<HashSet<(String, String)>> = OnceLock::new();
    ROUTES.get_or_init(|| {
        let doc = serde_json::to_value(crate::http::openapi()).unwrap_or_default();
        let mut out = HashSet::new();
        let Some(paths) = doc.get("paths").and_then(|p| p.as_object()) else {
            return out;
        };
        for (path, item) in paths {
            let Some(item) = item.as_object() else {
                continue;
            };
            for (method, op) in item {
                // No operation-level `security` means the document default (bearer or cookie).
                let authenticated = match op.get("security").and_then(|s| s.as_array()) {
                    None => true,
                    Some(reqs) => reqs
                        .iter()
                        .any(|r| r.get("bearerAuth").is_some() || r.get("cookieAuth").is_some()),
                };
                if authenticated {
                    out.insert((method.to_ascii_uppercase(), path.clone()));
                }
            }
        }
        out
    })
}

/// The per-IP policy for a request, or `None` when the per-user limit applies instead.
pub fn ip_policy(
    method: &Method,
    route: Option<&str>,
    path: &str,
    has_credentials: bool,
) -> Option<Policy> {
    if path.starts_with("/v1/auth/") {
        return Some(Policy::Auth);
    }
    if path.starts_with("/_storage/") {
        return Some(Policy::Storage);
    }
    if path == "/admin" || path.starts_with("/admin/") || path.starts_with("/docs") {
        return Some(Policy::Static);
    }
    let authenticated = route.is_some_and(|r| {
        authenticated_routes().contains(&(method.as_str().to_string(), r.to_string()))
    });
    if authenticated && has_credentials {
        None
    } else {
        Some(Policy::Public)
    }
}

/// Middleware applied to every route (after routing, so the matched template is known).
pub async fn ip_limits(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let (parts, body) = req.into_parts();
    let route = parts
        .extensions
        .get::<MatchedPath>()
        .map(MatchedPath::as_str);
    if let Some(policy) = ip_policy(
        &parts.method,
        route,
        parts.uri.path(),
        has_credentials(&parts.headers),
    ) && let Err(e) = check_ip(&state, &parts, policy)
    {
        return e.into_response();
    }
    next.run(Request::from_parts(parts, body)).await
}

/// Consumes one unit of `policy` for the client IP of `parts`.
pub fn check_ip(
    state: &AppState,
    parts: &axum::http::request::Parts,
    policy: Policy,
) -> Result<(), ApiError> {
    let ip = client_ip(parts, state.config.trust_proxy_headers)
        .map_or_else(|| "unknown".to_string(), |ip: IpAddr| ip.to_string());
    state.limits.check(policy, &format!("ip:{ip}"))
}

pub fn has_credentials(headers: &axum::http::HeaderMap) -> bool {
    headers.contains_key(axum::http::header::AUTHORIZATION)
        || headers
            .get(axum::http::header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.contains("__Host-vgames_session="))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_policy_allows_20_then_limits_with_retry_after() {
        let l = RateLimits::new();
        for _ in 0..20 {
            l.check(Policy::Auth, "ip:1.2.3.4").unwrap();
        }
        let e = l.check(Policy::Auth, "ip:1.2.3.4").unwrap_err();
        assert_eq!(e.code, "rate_limited");
        assert!(e.retry_after_secs.unwrap() >= 1);
        // Other keys are unaffected.
        l.check(Policy::Auth, "ip:5.6.7.8").unwrap();
    }

    #[test]
    fn check_n_counts_units() {
        let l = RateLimits::new();
        l.check_n(Policy::DownloadUrls, "u", 1500).unwrap();
        assert!(l.check_n(Policy::DownloadUrls, "u", 600).is_err());
        assert!(
            l.check_n(Policy::DownloadUrls, "v", 5000).is_err(),
            "above burst never passes"
        );
    }
}
