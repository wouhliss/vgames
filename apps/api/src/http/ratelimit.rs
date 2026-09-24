//! Rate limits (docs/architecture/01-security.md §4.4), per server instance.
//!
//! Keyed GCRA limiters from `governor`. Unauthenticated routes are limited by client IP
//! (middleware); authenticated requests by user (checked when the session resolves);
//! specific actions by the handler with [`RateLimits::check`].

use std::{
    net::IpAddr,
    num::NonZeroU32,
    sync::atomic::{AtomicU64, Ordering},
};

use axum::{
    extract::{Request, State},
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
}

impl Policy {
    pub const ALL: [Policy; 7] = [
        Policy::Auth,
        Policy::User,
        Policy::FriendRequests,
        Policy::Invites,
        Policy::Messages,
        Policy::DownloadUrls,
        Policy::Public,
    ];

    fn quota(self) -> Quota {
        let n = |v: u32| NonZeroU32::new(v).unwrap_or(NonZeroU32::MIN);
        match self {
            Policy::Auth => Quota::per_minute(n(20)),
            Policy::User => Quota::per_minute(n(600)),
            Policy::FriendRequests => Quota::per_hour(n(30)),
            Policy::Invites => Quota::per_hour(n(60)),
            Policy::Messages => Quota::per_minute(n(120)),
            Policy::DownloadUrls => Quota::per_hour(n(2000)),
            Policy::Public => Quota::per_minute(n(300)),
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
        Self {
            limiters: Policy::ALL
                .iter()
                .map(|p| (*p, RateLimiter::keyed(p.quota())))
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

/// Middleware applied to every route: `/v1/auth/*` is limited per IP under
/// [`Policy::Auth`]; other requests without credentials per IP under [`Policy::Public`].
/// Requests with credentials are limited per user when the session resolves
/// ([`Policy::User`]), so users behind one NAT do not share a budget.
pub async fn ip_limits(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let (parts, body) = req.into_parts();
    let path = parts.uri.path();
    let policy = if path.starts_with("/v1/auth/") {
        Some(Policy::Auth)
    } else if has_credentials(&parts.headers) {
        None
    } else {
        Some(Policy::Public)
    };
    if let Some(policy) = policy {
        let ip = client_ip(&parts, state.config.trust_proxy_headers)
            .map_or_else(|| "unknown".to_string(), |ip: IpAddr| ip.to_string());
        if let Err(e) = state.limits.check(policy, &format!("ip:{ip}")) {
            return e.into_response();
        }
    }
    next.run(Request::from_parts(parts, body)).await
}

fn has_credentials(headers: &axum::http::HeaderMap) -> bool {
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
