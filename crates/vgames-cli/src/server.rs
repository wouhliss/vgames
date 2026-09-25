//! Talking to a vgames server: which URLs are accepted, the HTTP client, and
//! problem+json errors.
//!
//! - HTTPS only; plain HTTP is accepted for `localhost` / loopback addresses
//!   (the local stack) and nothing else.
//! - Only the origin is kept (no path, query or credentials in the URL).
//! - No redirects are followed, so a bearer token never leaves the origin.
//! - Every request has a timeout and every response body a size cap.

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;
use zeroize::Zeroizing;

const TIMEOUT: Duration = Duration::from_secs(30);
/// Largest JSON response read (trust bundles are the largest documents).
const MAX_BODY: usize = 8 * 1024 * 1024;
const USER_AGENT: &str = concat!("vgames-cli/", env!("CARGO_PKG_VERSION"));

/// `--server URL`, shared by every server command.
#[derive(Debug, Clone, clap::Args)]
pub struct ServerArgs {
    /// The vgames server, e.g. https://games.example.com (plain http only for localhost).
    #[arg(long, env = "VGAMES_SERVER", value_name = "URL")]
    pub server: String,
}

impl ServerArgs {
    pub fn origin(&self) -> Result<Url> {
        origin(&self.server)
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        None => false,
    }
}

/// Validates a server URL and reduces it to its origin (`https://host[:port]`).
pub fn origin(s: &str) -> Result<Url> {
    let url = Url::parse(s.trim()).with_context(|| format!("{s:?} is not a URL"))?;
    match url.scheme() {
        "https" => {}
        "http" if is_loopback(&url) => {}
        "http" => bail!("{s} must use https (plain http is only accepted for localhost)"),
        other => bail!("{s}: unsupported scheme {other:?}"),
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("{s}: credentials in the URL are not accepted");
    }
    if url.host_str().is_none_or(str::is_empty) {
        bail!("{s}: no host");
    }
    let mut origin = url.clone();
    origin.set_path("/");
    origin.set_query(None);
    origin.set_fragment(None);
    Ok(origin)
}

/// A `problem+json` answer from the server.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Problem {
    #[serde(skip)]
    pub status: u16,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub errors: Vec<FieldProblem>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct FieldProblem {
    pub field: String,
    pub code: String,
    #[serde(default)]
    pub message: Option<String>,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let title = if self.title.is_empty() {
            "request refused"
        } else {
            &self.title
        };
        write!(f, "the server answered {}: {title}", self.status)?;
        if !self.code.is_empty() {
            write!(f, " ({})", self.code)?;
        }
        if let Some(d) = &self.detail {
            write!(f, ". {d}")?;
        }
        for e in &self.errors {
            write!(f, "\n  {}: {}", e.field, e.code)?;
            if let Some(m) = &e.message {
                write!(f, " ({m})")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for Problem {}

/// HTTP client bound to one server origin, optionally with a bearer token.
pub struct Api {
    http: reqwest::Client,
    origin: Url,
    token: Option<Zeroizing<String>>,
}

impl Api {
    pub fn new(origin: Url) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .https_only(!is_loopback(&origin))
            .build()
            .context("building the HTTP client")?;
        Ok(Self {
            http,
            origin,
            token: None,
        })
    }

    pub fn with_token(mut self, token: Zeroizing<String>) -> Self {
        self.token = Some(token);
        self
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.origin
            .join(path.trim_start_matches('/'))
            .with_context(|| format!("bad API path {path}"))
    }

    fn request(&self, method: reqwest::Method, path: &str) -> Result<reqwest::RequestBuilder> {
        let mut req = self
            .http
            .request(method, self.url(path)?)
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(t) = &self.token {
            req = req.bearer_auth(t.as_str());
        }
        Ok(req)
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.send(self.request(reqwest::Method::GET, path)?, path)
            .await
    }

    pub async fn post<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        self.send(self.request(reqwest::Method::POST, path)?.json(body), path)
            .await
    }

    /// POST without a response body (`204`).
    pub async fn post_empty(&self, path: &str) -> Result<()> {
        let response = self
            .request(reqwest::Method::POST, path)?
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        let status = response.status();
        let body = read_capped(response, path).await?;
        if !status.is_success() {
            return Err(problem(status.as_u16(), &body).into());
        }
        Ok(())
    }

    async fn send<T: DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
        path: &str,
    ) -> Result<T> {
        let response = req
            .send()
            .await
            .with_context(|| format!("cannot reach {}", self.origin))?;
        let status = response.status();
        let body = read_capped(response, path).await?;
        if !status.is_success() {
            return Err(problem(status.as_u16(), &body).into());
        }
        serde_json::from_slice(&body).with_context(|| format!("unexpected answer to {path}"))
    }
}

fn problem(status: u16, body: &[u8]) -> Problem {
    let mut p = serde_json::from_slice::<Problem>(body).unwrap_or(Problem {
        status,
        code: String::new(),
        title: String::new(),
        detail: None,
        errors: Vec::new(),
    });
    p.status = status;
    p
}

async fn read_capped(mut response: reqwest::Response, path: &str) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        bail!("the answer to {path} is larger than {MAX_BODY} bytes");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("reading the answer to {path}"))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_BODY {
            bail!("the answer to {path} is larger than {MAX_BODY} bytes");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The problem code of an API error, if the error is one.
pub fn problem_code(e: &anyhow::Error) -> Option<(u16, &str)> {
    e.downcast_ref::<Problem>()
        .map(|p| (p.status, p.code.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        let ok = |s: &str| origin(s).unwrap().to_string();
        assert_eq!(
            ok("https://games.example.com"),
            "https://games.example.com/"
        );
        assert_eq!(
            ok("https://games.example.com:8443/some/path?x=1#f"),
            "https://games.example.com:8443/"
        );
        assert_eq!(ok("http://localhost:8080"), "http://localhost:8080/");
        assert_eq!(ok("http://127.0.0.1:8080"), "http://127.0.0.1:8080/");
        assert_eq!(ok("http://[::1]:8080"), "http://[::1]:8080/");
        for bad in [
            "http://games.example.com",
            "http://10.0.0.1",
            "http://localhost.example.com",
            "ftp://games.example.com",
            "https://user:pass@games.example.com",
            "games.example.com",
            "file:///etc/passwd",
        ] {
            assert!(origin(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn problems_display_code_detail_and_fields() {
        let p = problem(
            422,
            br#"{"type":"urn:vgames:problem:unknown_holder","title":"Unknown holder","status":422,
                "code":"unknown_holder","errors":[{"field":"publishers[0].holder_user_id","code":"unknown"}]}"#,
        );
        let text = p.to_string();
        assert!(text.contains("422"), "{text}");
        assert!(text.contains("unknown_holder"), "{text}");
        assert!(text.contains("publishers[0].holder_user_id"), "{text}");
        assert_eq!(problem(502, b"<html>").status, 502);
    }
}
