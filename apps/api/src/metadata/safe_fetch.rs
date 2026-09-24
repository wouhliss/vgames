//! SSRF-safe download of provider images (01-security §5): HTTPS only, host allowlist,
//! a resolver that refuses private, loopback and link-local addresses, same-host redirects
//! only, a byte cap and a timeout.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::{
    StatusCode,
    dns::{Addrs, Name, Resolve, Resolving},
    header,
};
use url::Url;

/// Hosts provider images may come from.
pub const IMAGE_HOSTS: &[&str] = &[
    "images.igdb.com",
    "shared.akamai.steamstatic.com",
    "cdn.akamai.steamstatic.com",
    "shared.cloudflare.steamstatic.com",
    "cdn.cloudflare.steamstatic.com",
];

const MAX_REDIRECTS: usize = 3;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FetchError {
    /// The URL or a redirect breaks the policy. Never retried.
    #[error("blocked: {0}")]
    Blocked(String),
    /// The server answered with a client error (e.g. 404). Never retried.
    #[error("http status {0}")]
    Status(u16),
    #[error("response larger than {0} bytes")]
    TooLarge(usize),
    /// Network trouble or a server error; worth retrying later.
    #[error("transient: {0}")]
    Transient(String),
}

impl FetchError {
    pub fn is_transient(&self) -> bool {
        matches!(self, FetchError::Transient(_))
    }
}

/// Which addresses a resolved host may have.
pub type AddressFilter = fn(IpAddr) -> bool;

#[derive(Clone)]
pub struct FetchPolicy {
    pub allowed_hosts: Vec<String>,
    pub https_only: bool,
    pub max_bytes: usize,
    pub timeout: Duration,
    pub address_allowed: AddressFilter,
    /// Fixed answers for host names (tests); still subject to `address_allowed`.
    pub resolve_overrides: HashMap<String, Vec<SocketAddr>>,
}

impl FetchPolicy {
    pub fn production() -> Self {
        Self {
            allowed_hosts: IMAGE_HOSTS.iter().map(|h| (*h).to_string()).collect(),
            https_only: true,
            max_bytes: 10 * 1024 * 1024,
            timeout: Duration::from_secs(15),
            address_allowed: is_public,
            resolve_overrides: HashMap::new(),
        }
    }
}

/// Fetches images under a [`FetchPolicy`].
#[derive(Clone)]
pub struct SafeFetcher {
    client: reqwest::Client,
    policy: Arc<FetchPolicy>,
}

impl SafeFetcher {
    pub fn new(policy: FetchPolicy) -> Result<Self, reqwest::Error> {
        let resolver = GuardedResolver {
            overrides: Arc::new(policy.resolve_overrides.clone()),
            allowed: policy.address_allowed,
        };
        let client = reqwest::Client::builder()
            .no_proxy()
            .dns_resolver(Arc::new(resolver))
            .redirect(reqwest::redirect::Policy::none())
            .timeout(policy.timeout)
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("vgames-api/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            policy: Arc::new(policy),
        })
    }

    /// Checks scheme, host and port against the policy (before any network access).
    pub fn check_url(&self, url: &Url) -> Result<(), FetchError> {
        let https = url.scheme() == "https";
        if !https && (self.policy.https_only || url.scheme() != "http") {
            return Err(FetchError::Blocked(format!("scheme {}", url.scheme())));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(FetchError::Blocked("credentials in URL".into()));
        }
        let Some(url::Host::Domain(host)) = url.host() else {
            return Err(FetchError::Blocked("host must be a name".into()));
        };
        if !self
            .policy
            .allowed_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(host))
        {
            return Err(FetchError::Blocked(format!("host {host} is not allowed")));
        }
        if self.policy.https_only && url.port().is_some_and(|p| p != 443) {
            return Err(FetchError::Blocked("non-standard port".into()));
        }
        Ok(())
    }

    /// Downloads `url`, following at most three same-host redirects.
    pub async fn get(&self, url: &str) -> Result<Bytes, FetchError> {
        let mut url =
            Url::parse(url).map_err(|e| FetchError::Blocked(format!("invalid URL: {e}")))?;
        self.check_url(&url)?;
        let origin_host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        for _ in 0..=MAX_REDIRECTS {
            let resp = self
                .client
                .get(url.clone())
                .send()
                .await
                .map_err(classify)?;
            let status = resp.status();
            if status.is_redirection() {
                let next = resp
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| FetchError::Blocked("redirect without Location".into()))?;
                let next = url
                    .join(next)
                    .map_err(|e| FetchError::Blocked(format!("invalid redirect: {e}")))?;
                self.check_url(&next)?;
                if !next
                    .host_str()
                    .is_some_and(|h| h.eq_ignore_ascii_case(&origin_host))
                    || next.scheme() != url.scheme()
                {
                    return Err(FetchError::Blocked("cross-host redirect".into()));
                }
                url = next;
                continue;
            }
            if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
                return Err(FetchError::Transient(format!(
                    "http status {}",
                    status.as_u16()
                )));
            }
            if !status.is_success() {
                return Err(FetchError::Status(status.as_u16()));
            }
            return read_capped(resp, self.policy.max_bytes).await;
        }
        Err(FetchError::Blocked("too many redirects".into()))
    }
}

fn classify(e: reqwest::Error) -> FetchError {
    // The resolver's refusal surfaces as a connect error; report it as a policy block.
    let mut src: Option<&(dyn std::error::Error + 'static)> = Some(&e);
    while let Some(s) = src {
        if let Some(b) = s.downcast_ref::<AddressBlocked>() {
            return FetchError::Blocked(b.to_string());
        }
        src = s.source();
    }
    FetchError::Transient(e.without_url().to_string())
}

/// Reads a body of at most `max` bytes.
pub async fn read_capped(resp: reqwest::Response, max: usize) -> Result<Bytes, FetchError> {
    if resp.content_length().is_some_and(|n| n > max as u64) {
        return Err(FetchError::TooLarge(max));
    }
    let mut buf = BytesMut::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| FetchError::Transient(e.without_url().to_string()))?;
        if buf.len() + chunk.len() > max {
            return Err(FetchError::TooLarge(max));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf.freeze())
}

#[derive(Debug, thiserror::Error)]
#[error("{host} resolves to a disallowed address")]
struct AddressBlocked {
    host: String,
}

struct GuardedResolver {
    overrides: Arc<HashMap<String, Vec<SocketAddr>>>,
    allowed: AddressFilter,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_ascii_lowercase();
        let overrides = self.overrides.clone();
        let allowed = self.allowed;
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = match overrides.get(&host) {
                Some(a) => a.clone(),
                None => tokio::net::lookup_host((host.as_str(), 0)).await?.collect(),
            };
            // One bad address poisons the answer: DNS rebinding tricks mix public and private ones.
            if addrs.is_empty() || addrs.iter().any(|a| !allowed(a.ip())) {
                return Err(
                    Box::new(AddressBlocked { host }) as Box<dyn std::error::Error + Send + Sync>
                );
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// Globally routable unicast addresses only.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let seg0 = v6.segments()[0];
            !(v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || (seg0 & 0xfe00) == 0xfc00 // unique local
                || (seg0 & 0xffc0) == 0xfe80 // link-local
                || (seg0 & 0xffc0) == 0xfec0 // site-local (deprecated)
                || seg0 == 0x2001 && v6.segments()[1] == 0x0db8 // documentation
                || seg0 == 0x0064 && v6.segments()[1] == 0xff9b // NAT64
                || seg0 == 0x2002 // 6to4 (embeds arbitrary IPv4)
                || v6 == Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0, 0))
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // CGNAT 100.64/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // benchmarking 198.18/15
        || o[0] >= 240)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_classes() {
        for bad in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "198.18.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fc00::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "2002:7f00:1::",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_public(bad.parse().unwrap()), "{bad} must be blocked");
        }
        for good in ["8.8.8.8", "151.101.1.1", "2606:4700::1111"] {
            assert!(is_public(good.parse().unwrap()), "{good} must be allowed");
        }
    }

    #[test]
    fn url_checks() {
        let f = SafeFetcher::new(FetchPolicy::production()).unwrap();
        let check = |u: &str| f.check_url(&Url::parse(u).unwrap());
        assert!(check("https://images.igdb.com/igdb/image/upload/t_cover_big/abc.jpg").is_ok());
        assert!(check("https://IMAGES.igdb.com/x.jpg").is_ok());
        for bad in [
            "http://images.igdb.com/x.jpg",
            "https://evil.example/x.jpg",
            "https://images.igdb.com.evil.example/x.jpg",
            "https://127.0.0.1/x.jpg",
            "https://[::1]/x.jpg",
            "https://user:pw@images.igdb.com/x.jpg",
            "https://images.igdb.com:8443/x.jpg",
            "file:///etc/passwd",
        ] {
            assert!(matches!(check(bad), Err(FetchError::Blocked(_))), "{bad}");
        }
    }

    #[tokio::test]
    async fn resolver_refuses_private_answers() {
        let mut policy = FetchPolicy::production();
        policy.resolve_overrides.insert(
            "images.igdb.com".into(),
            vec!["10.0.0.7:443".parse().unwrap()],
        );
        let f = SafeFetcher::new(policy).unwrap();
        let err = f
            .get("https://images.igdb.com/igdb/image/upload/t_1080p/x.jpg")
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Blocked(_)), "{err:?}");

        let mut policy = FetchPolicy::production();
        policy.resolve_overrides.insert(
            "images.igdb.com".into(),
            vec![
                "8.8.8.8:443".parse().unwrap(),
                "127.0.0.1:443".parse().unwrap(),
            ],
        );
        let f = SafeFetcher::new(policy).unwrap();
        let err = f.get("https://images.igdb.com/x.jpg").await.unwrap_err();
        assert!(matches!(err, FetchError::Blocked(_)), "{err:?}");
    }
}
