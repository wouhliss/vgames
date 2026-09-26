//! Server URLs and the discovery document (`/.well-known/vgames.json`).

use std::time::Duration;

use serde::Deserialize;
use url::Url;
use uuid::Uuid;
use vgames_core::sign::{Fingerprint, PublicKey};

use super::{RegistrationMode, ServerError};
use crate::api::{ApiError, read_capped};

/// Discovery is interactive: give up quickly.
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest discovery document accepted.
pub const MAX_DISCOVERY_BYTES: usize = 64 * 1024;
const FORMAT: &str = "vgames.server/1";
const MAX_URL_CHARS: usize = 2048;

/// Normalizes what the user typed (or a link carried) to a server base URL:
/// `https://host[:port]/`. A missing scheme means HTTPS. Plain HTTP is refused,
/// except for loopback hosts when `allow_loopback_http` (debug builds).
pub fn normalize_url(input: &str, allow_loopback_http: bool) -> Result<Url, ServerError> {
    let input = input.trim();
    if input.is_empty() || input.len() > MAX_URL_CHARS || input.chars().any(char::is_whitespace) {
        return Err(ServerError::InvalidUrl);
    }
    let with_scheme = if input.contains("://") {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    let url = Url::parse(&with_scheme).map_err(|_| ServerError::InvalidUrl)?;
    let host = url.host().ok_or(ServerError::InvalidUrl)?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ServerError::InvalidUrl);
    }
    match url.scheme() {
        "https" => {}
        "http" => {
            let loopback = match &host {
                url::Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
                url::Host::Ipv4(ip) => ip.is_loopback(),
                url::Host::Ipv6(ip) => ip.is_loopback(),
            };
            if !(allow_loopback_http && loopback) {
                return Err(ServerError::InsecureScheme);
            }
        }
        _ => return Err(ServerError::InvalidUrl),
    }
    // The server lives at the origin: path, query and fragment are dropped.
    let mut base = url;
    base.set_path("/");
    base.set_query(None);
    base.set_fragment(None);
    Ok(base)
}

/// The discovery fields the launcher uses. Parsed leniently (unknown API
/// versions, features and modes are ignored) so a newer server still reads.
#[derive(Debug, Deserialize)]
struct WellKnown {
    format: String,
    server_id: String,
    name: String,
    #[serde(default)]
    motd: Option<String>,
    #[serde(default)]
    api_versions: Vec<String>,
    root_public_key: String,
    root_key_fingerprint: String,
    #[serde(default)]
    registration_mode: Option<String>,
    #[serde(default)]
    min_launcher_version: Option<String>,
}

/// A validated discovery document.
#[derive(Debug, Clone)]
pub struct Discovered {
    pub server_id: Uuid,
    pub name: String,
    pub motd: Option<String>,
    pub root: PublicKey,
    pub fingerprint: Fingerprint,
    pub registration_mode: Option<RegistrationMode>,
}

/// Fetches and validates the discovery document of `base`.
pub async fn discover(
    http: &reqwest::Client,
    base: &Url,
    launcher_version: &semver::Version,
) -> Result<Discovered, ServerError> {
    let url = base
        .join(".well-known/vgames.json")
        .map_err(|_| ServerError::InvalidUrl)?;
    let response = http
        .get(url)
        .timeout(DISCOVERY_TIMEOUT)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| ServerError::from_api(ApiError::from_reqwest(&e)))?;
    let status = response.status();
    if status.is_server_error() {
        return Err(ServerError::Unreachable {
            detail: format!("the server answered HTTP {}", status.as_u16()),
        });
    }
    if !status.is_success() {
        return Err(ServerError::NotVgames);
    }
    let body = match read_capped(response, MAX_DISCOVERY_BYTES).await {
        Ok(body) => body,
        Err(ApiError::InvalidResponse(_)) => return Err(ServerError::NotVgames),
        Err(error) => return Err(ServerError::from_api(error)),
    };
    let doc: WellKnown = serde_json::from_slice(&body).map_err(|_| ServerError::NotVgames)?;
    validate(doc, launcher_version)
}

fn validate(doc: WellKnown, launcher_version: &semver::Version) -> Result<Discovered, ServerError> {
    if doc.format != FORMAT {
        return Err(ServerError::NotVgames);
    }
    let server_id =
        vgames_core::trust::parse_server_id(&doc.server_id).ok_or(ServerError::NotVgames)?;
    let root = PublicKey::from_base64(&doc.root_public_key).map_err(|_| ServerError::NotVgames)?;
    let fingerprint = root.fingerprint();
    // The published fingerprint must describe the published key.
    let published: Fingerprint = doc
        .root_key_fingerprint
        .parse()
        .map_err(|_| ServerError::NotVgames)?;
    if published != fingerprint {
        return Err(ServerError::NotVgames);
    }
    let too_old = |min: &str| ServerError::LauncherTooOld {
        min_version: min.to_owned(),
        current_version: launcher_version.to_string(),
    };
    if let Some(min) = doc.min_launcher_version.as_deref() {
        let parsed = semver::Version::parse(min).map_err(|_| ServerError::NotVgames)?;
        if *launcher_version < parsed {
            return Err(too_old(min));
        }
    }
    if !doc.api_versions.iter().any(|v| v == "v1") {
        return Err(too_old(
            doc.min_launcher_version.as_deref().unwrap_or("newer"),
        ));
    }
    let name = clean_text(&doc.name, 100).ok_or(ServerError::NotVgames)?;
    let registration_mode = match doc.registration_mode.as_deref() {
        Some("open") => Some(RegistrationMode::Open),
        Some("allowlist") => Some(RegistrationMode::Allowlist),
        Some("closed") => Some(RegistrationMode::Closed),
        _ => None,
    };
    Ok(Discovered {
        server_id,
        name,
        motd: doc.motd.as_deref().and_then(|m| clean_text(m, 500)),
        root,
        fingerprint,
        registration_mode,
    })
}

/// Server-provided display text: control characters removed, length clamped.
fn clean_text(text: &str, max_chars: usize) -> Option<String> {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(max_chars)
        .collect();
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_normalize_to_the_origin() {
        let url = normalize_url("  games.example.com/some/path?x=1#f ", false).unwrap();
        assert_eq!(url.as_str(), "https://games.example.com/");
        let url = normalize_url("https://Games.Example.com:8443", false).unwrap();
        assert_eq!(url.as_str(), "https://games.example.com:8443/");
    }

    #[test]
    fn plain_http_is_refused_except_loopback_in_debug() {
        assert_eq!(
            normalize_url("http://games.example.com", true),
            Err(ServerError::InsecureScheme)
        );
        assert_eq!(
            normalize_url("http://localhost:8080", false),
            Err(ServerError::InsecureScheme)
        );
        assert!(normalize_url("http://localhost:8080", true).is_ok());
        assert!(normalize_url("http://127.0.0.1:8080", true).is_ok());
        assert!(normalize_url("http://[::1]:8080", true).is_ok());
    }

    #[test]
    fn malformed_urls_are_refused() {
        for input in [
            "",
            "   ",
            "ftp://games.example.com",
            "file:///etc/passwd",
            "https://user:pw@games.example.com",
            "https://games example.com",
            "javascript:alert(1)",
        ] {
            assert_eq!(
                normalize_url(input, true),
                Err(ServerError::InvalidUrl),
                "{input:?}"
            );
        }
    }

    fn doc(key: &PublicKey) -> WellKnown {
        WellKnown {
            format: FORMAT.into(),
            server_id: "01920000-0000-7000-8000-000000000000".into(),
            name: "  Friends\u{7} ".into(),
            motd: None,
            api_versions: vec!["v1".into(), "v2".into()],
            root_public_key: key.to_base64(),
            root_key_fingerprint: key.fingerprint().to_string(),
            registration_mode: Some("allowlist".into()),
            min_launcher_version: Some("0.1.0".into()),
        }
    }

    fn key(seed: u8) -> PublicKey {
        vgames_core::sign::SecretKey::from_seed(&[seed; 32]).public_key()
    }

    #[test]
    fn documents_are_validated() {
        let v = semver::Version::new(1, 0, 0);
        let ok = validate(doc(&key(1)), &v).unwrap();
        assert_eq!(ok.name, "Friends");
        assert_eq!(ok.registration_mode, Some(RegistrationMode::Allowlist));

        let mut wrong_fp = doc(&key(1));
        wrong_fp.root_key_fingerprint = key(2).fingerprint().to_string();
        assert_eq!(validate(wrong_fp, &v).unwrap_err(), ServerError::NotVgames);

        let mut too_new = doc(&key(1));
        too_new.min_launcher_version = Some("2.0.0".into());
        assert!(matches!(
            validate(too_new, &v),
            Err(ServerError::LauncherTooOld { .. })
        ));

        let mut no_v1 = doc(&key(1));
        no_v1.api_versions = vec!["v2".into()];
        assert!(matches!(
            validate(no_v1, &v),
            Err(ServerError::LauncherTooOld { .. })
        ));

        let mut other = doc(&key(1));
        other.format = "something/1".into();
        assert_eq!(validate(other, &v).unwrap_err(), ServerError::NotVgames);
    }
}
