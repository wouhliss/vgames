//! Configuration from environment variables (`.env.example` is the reference).
//!
//! Every variable is parsed and validated at startup. All problems are collected
//! and reported together, and the process exits with code 1: no silent defaults
//! for secrets.

use std::{fmt, net::SocketAddr, path::PathBuf, time::Duration};

use base64::Engine as _;
use url::Url;
use uuid::Uuid;

use crate::secret::Secret;

/// Parsed, validated configuration.
#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: SocketAddr,
    /// External origin, without a trailing slash (e.g. `https://vgames.example`).
    pub public_url: Url,
    pub server_name: String,
    pub server_id: Uuid,
    pub database_url: Secret<String>,
    pub database_migration_url: Secret<String>,
    pub db_max_connections: u32,
    pub log_filter: String,
    pub log_format: LogFormat,
    /// Ed25519 public half of the offline root key.
    pub root_public_key: vgames_core::PublicKey,
    /// Server-side secret for MACs (cursors, fs storage URLs, tickets). ≥ 32 bytes.
    pub server_secret: Secret<Vec<u8>>,
    pub discord: DiscordConfig,
    pub bootstrap_owner_discord_id: Option<String>,
    pub storage: StorageConfig,
    pub igdb: Option<IgdbConfig>,
    pub steam_metadata_enabled: bool,
    pub save_quota_bytes_per_package: u64,
    pub signed_url_ttl: Duration,
    pub worker_concurrency: usize,
    /// Honour `X-Forwarded-For` for client IPs (only behind a trusted proxy).
    pub trust_proxy_headers: bool,
    /// Built admin SPA to serve at `/admin/` (A1-T15).
    pub admin_dist: Option<PathBuf>,
}

impl Config {
    /// `true` when the public URL is served over HTTPS (cookies get `Secure`, HSTS is sent).
    pub fn is_https(&self) -> bool {
        self.public_url.scheme() == "https"
    }

    /// The public origin as a string without a trailing slash.
    pub fn public_origin(&self) -> String {
        self.public_url.as_str().trim_end_matches('/').to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,
    Json,
}

#[derive(Clone, Debug)]
pub enum DiscordConfig {
    Discord(Box<DiscordApp>),
    /// Debug builds only (`VGAMES_DEV_FAKE_DISCORD=1`, localhost public URL).
    Fake,
}

#[derive(Clone, Debug)]
pub struct DiscordApp {
    pub client_id: String,
    pub client_secret: Secret<String>,
    pub redirect_uri: Url,
    /// Overridable for tests (wiremock). Defaults to `https://discord.com`.
    pub api_base: Url,
}

#[derive(Clone, Debug)]
pub enum StorageConfig {
    Fs {
        root: PathBuf,
        signing_key: Secret<Vec<u8>>,
    },
    Gcs {
        bucket_packages: String,
        bucket_saves: String,
        bucket_assets: String,
        credentials_file: Option<PathBuf>,
        emulator_host: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct IgdbConfig {
    pub client_id: String,
    pub client_secret: Secret<String>,
}

/// Every problem found while reading the configuration.
#[derive(Debug, PartialEq, Eq)]
pub struct ConfigErrors(pub Vec<String>);

impl fmt::Display for ConfigErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "invalid configuration ({} problem(s)):", self.0.len())?;
        for e in &self.0 {
            writeln!(f, "  - {e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigErrors {}

impl Config {
    /// Reads the process environment.
    pub fn from_env() -> Result<Self, ConfigErrors> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Reads configuration through `get` (tests pass a map).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigErrors> {
        let mut r = Reader {
            get: &get,
            errors: Vec::new(),
        };

        let bind_addr = r.parse_or("VGAMES_BIND_ADDR", "127.0.0.1:8080", |v| {
            v.parse::<SocketAddr>().map_err(|e| e.to_string())
        });
        let public_url = r
            .required("VGAMES_PUBLIC_URL")
            .and_then(|v| r.check("VGAMES_PUBLIC_URL", parse_public_url(&v)));
        let server_name = r
            .optional("VGAMES_SERVER_NAME")
            .unwrap_or_else(|| "vgames".to_string());
        if server_name.chars().count() > 100 {
            r.error("VGAMES_SERVER_NAME", "must be at most 100 characters");
        }
        let server_id = r.required("VGAMES_SERVER_ID").and_then(|v| {
            r.check(
                "VGAMES_SERVER_ID",
                Uuid::parse_str(&v).map_err(|e| e.to_string()),
            )
        });
        let database_url = r.required("DATABASE_URL");
        if let Some(u) = &database_url
            && !(u.starts_with("postgres://") || u.starts_with("postgresql://"))
        {
            r.error("DATABASE_URL", "must be a postgres:// URL");
        }
        let database_migration_url = r
            .optional("DATABASE_MIGRATION_URL")
            .or_else(|| database_url.clone());
        let db_max_connections = r.parse_or("VGAMES_DB_MAX_CONNECTIONS", "20", |v| {
            v.parse::<u32>().map_err(|e| e.to_string()).and_then(|n| {
                if (1..=500).contains(&n) {
                    Ok(n)
                } else {
                    Err("must be 1..=500".into())
                }
            })
        });
        let log_filter = r
            .optional("VGAMES_LOG")
            .unwrap_or_else(|| "info".to_string());
        let log_format = r.parse_or("VGAMES_LOG_FORMAT", "pretty", |v| match v {
            "pretty" => Ok(LogFormat::Pretty),
            "json" => Ok(LogFormat::Json),
            _ => Err("must be `pretty` or `json`".into()),
        });
        let root_public_key = r.required("VGAMES_ROOT_PUBLIC_KEY").and_then(|v| {
            r.check(
                "VGAMES_ROOT_PUBLIC_KEY",
                decode_ed25519_public_key(&v)
                    .map_err(|e| format!("{e} (create one offline with `vgames keys init-root`)")),
            )
        });
        let server_secret = r
            .required("VGAMES_SERVER_SECRET")
            .and_then(|v| r.check("VGAMES_SERVER_SECRET", decode_secret_bytes(&v, 32)));

        let fake_discord = r.flag("VGAMES_DEV_FAKE_DISCORD");
        let discord = if fake_discord == Some(true) {
            if !cfg!(debug_assertions) {
                r.error(
                    "VGAMES_DEV_FAKE_DISCORD",
                    "is only available in debug builds",
                );
                None
            } else if let Some(u) = &public_url {
                if is_local_host(u) {
                    Some(DiscordConfig::Fake)
                } else {
                    r.error(
                        "VGAMES_DEV_FAKE_DISCORD",
                        "requires VGAMES_PUBLIC_URL to be localhost or 127.0.0.1",
                    );
                    None
                }
            } else {
                None
            }
        } else {
            let client_id = r.required("DISCORD_CLIENT_ID");
            if let Some(id) = &client_id
                && !is_snowflake(id)
            {
                r.error(
                    "DISCORD_CLIENT_ID",
                    "must be a Discord application id (digits)",
                );
            }
            let client_secret = r.required("DISCORD_CLIENT_SECRET");
            let redirect_uri = r.required("DISCORD_REDIRECT_URI").and_then(|v| {
                r.check(
                    "DISCORD_REDIRECT_URI",
                    Url::parse(&v).map_err(|e| e.to_string()),
                )
            });
            if let (Some(redirect), Some(public)) = (&redirect_uri, &public_url) {
                let expected = format!(
                    "{}/v1/auth/discord/callback",
                    public.as_str().trim_end_matches('/')
                );
                if redirect.as_str() != expected {
                    r.error(
                        "DISCORD_REDIRECT_URI",
                        &format!("must be exactly {expected}"),
                    );
                }
            }
            let api_base = r.parse_or("DISCORD_API_BASE", "https://discord.com", |v| {
                Url::parse(v).map_err(|e| e.to_string())
            });
            match (client_id, client_secret, redirect_uri, api_base) {
                (Some(client_id), Some(client_secret), Some(redirect_uri), Some(api_base)) => {
                    Some(DiscordConfig::Discord(Box::new(DiscordApp {
                        client_id,
                        client_secret: Secret::new(client_secret),
                        redirect_uri,
                        api_base,
                    })))
                }
                _ => None,
            }
        };

        let bootstrap_owner_discord_id = r.optional("VGAMES_BOOTSTRAP_OWNER_DISCORD_ID");
        if let Some(id) = &bootstrap_owner_discord_id
            && !is_snowflake(id)
        {
            r.error(
                "VGAMES_BOOTSTRAP_OWNER_DISCORD_ID",
                "must be a Discord user id (5-25 digits)",
            );
        }

        let storage = match r.required("VGAMES_STORAGE_BACKEND").as_deref() {
            Some("fs") => {
                let root = r.required("VGAMES_FS_STORAGE_ROOT").map(PathBuf::from);
                let key = r.required("VGAMES_FS_URL_SIGNING_KEY").and_then(|v| {
                    r.check("VGAMES_FS_URL_SIGNING_KEY", decode_secret_bytes(&v, 32))
                });
                match (root, key) {
                    (Some(root), Some(key)) => Some(StorageConfig::Fs {
                        root,
                        signing_key: Secret::new(key),
                    }),
                    _ => None,
                }
            }
            Some("gcs") => {
                let p = r.required("GCS_BUCKET_PACKAGES");
                let s = r.required("GCS_BUCKET_SAVES");
                let a = r.required("GCS_BUCKET_ASSETS");
                for (name, v) in [
                    ("GCS_BUCKET_PACKAGES", &p),
                    ("GCS_BUCKET_SAVES", &s),
                    ("GCS_BUCKET_ASSETS", &a),
                ] {
                    if let Some(b) = v
                        && !is_bucket_name(b)
                    {
                        r.error(name, "is not a valid bucket name");
                    }
                }
                let credentials_file = r
                    .optional("GOOGLE_APPLICATION_CREDENTIALS")
                    .map(PathBuf::from);
                let emulator_host = r.optional("STORAGE_EMULATOR_HOST");
                match (p, s, a) {
                    (Some(bucket_packages), Some(bucket_saves), Some(bucket_assets)) => {
                        Some(StorageConfig::Gcs {
                            bucket_packages,
                            bucket_saves,
                            bucket_assets,
                            credentials_file,
                            emulator_host,
                        })
                    }
                    _ => None,
                }
            }
            Some(_) => {
                r.error("VGAMES_STORAGE_BACKEND", "must be `gcs` or `fs`");
                None
            }
            None => None,
        };

        let igdb = match (
            r.optional("IGDB_CLIENT_ID"),
            r.optional("IGDB_CLIENT_SECRET"),
        ) {
            (Some(client_id), Some(secret)) => Some(IgdbConfig {
                client_id,
                client_secret: Secret::new(secret),
            }),
            (None, None) => None,
            _ => {
                r.error(
                    "IGDB_CLIENT_ID",
                    "IGDB_CLIENT_ID and IGDB_CLIENT_SECRET must be set together",
                );
                None
            }
        };
        let steam_metadata_enabled = r.flag("VGAMES_METADATA_STEAM_ENABLED").unwrap_or(true);
        let save_quota_bytes_per_package =
            r.parse_or("VGAMES_SAVE_QUOTA_BYTES_PER_PACKAGE", "536870912", |v| {
                v.parse::<u64>().map_err(|e| e.to_string()).and_then(|n| {
                    if n >= 1024 * 1024 {
                        Ok(n)
                    } else {
                        Err("must be at least 1048576".into())
                    }
                })
            });
        let signed_url_ttl = r.parse_or("VGAMES_SIGNED_URL_TTL_SECONDS", "21600", |v| {
            v.parse::<u64>().map_err(|e| e.to_string()).and_then(|n| {
                // GCS V4 signed URLs are valid for at most 7 days.
                if (60..=604_800).contains(&n) {
                    Ok(Duration::from_secs(n))
                } else {
                    Err("must be 60..=604800".into())
                }
            })
        });
        let worker_concurrency = r.parse_or("VGAMES_WORKER_CONCURRENCY", "4", |v| {
            v.parse::<usize>().map_err(|e| e.to_string()).and_then(|n| {
                if (1..=64).contains(&n) {
                    Ok(n)
                } else {
                    Err("must be 1..=64".into())
                }
            })
        });
        let trust_proxy_headers = r.flag("VGAMES_TRUST_PROXY_HEADERS").unwrap_or(false);
        let admin_dist = match r.optional("VGAMES_ADMIN_DIST") {
            Some(raw) => {
                let res = check_admin_dist(&raw);
                r.check("VGAMES_ADMIN_DIST", res)
            }
            None => None,
        };

        if !r.errors.is_empty() {
            return Err(ConfigErrors(r.errors));
        }
        match (
            bind_addr,
            public_url,
            server_id,
            database_url,
            database_migration_url,
            db_max_connections,
            log_format,
            root_public_key,
            server_secret,
            discord,
            storage,
            save_quota_bytes_per_package,
            signed_url_ttl,
            worker_concurrency,
        ) {
            (
                Some(bind_addr),
                Some(public_url),
                Some(server_id),
                Some(database_url),
                Some(database_migration_url),
                Some(db_max_connections),
                Some(log_format),
                Some(root_public_key),
                Some(server_secret),
                Some(discord),
                Some(storage),
                Some(save_quota_bytes_per_package),
                Some(signed_url_ttl),
                Some(worker_concurrency),
            ) => Ok(Config {
                bind_addr,
                public_url,
                server_name,
                server_id,
                database_url: Secret::new(database_url),
                database_migration_url: Secret::new(database_migration_url),
                db_max_connections,
                log_filter,
                log_format,
                root_public_key,
                server_secret: Secret::new(server_secret),
                discord,
                bootstrap_owner_discord_id,
                storage,
                igdb,
                steam_metadata_enabled,
                save_quota_bytes_per_package,
                signed_url_ttl,
                worker_concurrency,
                trust_proxy_headers,
                admin_dist,
            }),
            // Every `None` above recorded an error, so this arm is unreachable in practice.
            _ => Err(ConfigErrors(vec!["incomplete configuration".to_string()])),
        }
    }
}

struct Reader<'a, F: Fn(&str) -> Option<String>> {
    get: &'a F,
    errors: Vec<String>,
}

impl<F: Fn(&str) -> Option<String>> Reader<'_, F> {
    fn optional(&self, key: &str) -> Option<String> {
        (self.get)(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    fn required(&mut self, key: &str) -> Option<String> {
        let v = self.optional(key);
        if v.is_none() {
            self.errors.push(format!("{key} is required"));
        }
        v
    }

    fn error(&mut self, key: &str, msg: &str) {
        self.errors.push(format!("{key} {msg}"));
    }

    fn check<T>(&mut self, key: &str, res: Result<T, String>) -> Option<T> {
        match res {
            Ok(v) => Some(v),
            Err(e) => {
                self.errors.push(format!("{key} is invalid: {e}"));
                None
            }
        }
    }

    fn parse_or<T>(
        &mut self,
        key: &str,
        default: &str,
        parse: impl Fn(&str) -> Result<T, String>,
    ) -> Option<T> {
        let raw = self.optional(key).unwrap_or_else(|| default.to_string());
        let res = parse(&raw);
        self.check(key, res)
    }

    fn flag(&mut self, key: &str) -> Option<bool> {
        match self.optional(key)?.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => {
                self.errors
                    .push(format!("{key} is invalid: must be true or false"));
                None
            }
        }
    }
}

fn parse_public_url(v: &str) -> Result<Url, String> {
    let url = Url::parse(v).map_err(|e| e.to_string())?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return Err("must be an http(s) URL".into());
    }
    if url.scheme() == "http" && !is_local_host(&url) {
        return Err("must use https unless the host is localhost".into());
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err("must be an origin without path, query or fragment".into());
    }
    Ok(url)
}

/// The built admin UI: an existing directory with `index.html`, returned canonicalized so
/// served paths can be checked against it after resolving symlinks.
fn check_admin_dist(v: &str) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(v).map_err(|e| format!("cannot open {v}: {e}"))?;
    if !root.join("index.html").is_file() {
        return Err("must be the built admin UI directory (with index.html)".into());
    }
    Ok(root)
}

fn is_local_host(url: &Url) -> bool {
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
}

fn is_snowflake(v: &str) -> bool {
    (5..=25).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_digit())
}

fn is_bucket_name(v: &str) -> bool {
    (3..=63).contains(&v.len())
        && v.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_' || b == b'.'
        })
        && v.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
}

fn decode_b64(v: &str) -> Result<Vec<u8>, String> {
    let engines = [
        &base64::engine::general_purpose::STANDARD,
        &base64::engine::general_purpose::STANDARD_NO_PAD,
        &base64::engine::general_purpose::URL_SAFE,
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ];
    engines
        .iter()
        .find_map(|e| e.decode(v).ok())
        .ok_or_else(|| "is not valid base64".to_string())
}

/// Decodes and validates a base64 Ed25519 public key.
/// Decodes a root public key; `vgames-core` refuses off-curve and small-order points.
pub fn decode_ed25519_public_key(v: &str) -> Result<vgames_core::PublicKey, String> {
    let bytes = decode_b64(v)?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("must decode to 32 bytes, got {}", bytes.len()))?;
    vgames_core::PublicKey::from_bytes(&arr)
        .map_err(|_| "is not a valid Ed25519 public key".to_string())
}

fn decode_secret_bytes(v: &str, min_len: usize) -> Result<Vec<u8>, String> {
    let bytes = decode_b64(v)?;
    if bytes.len() < min_len {
        return Err(format!(
            "must decode to at least {min_len} bytes, got {}",
            bytes.len()
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// Freshly generated public key whose private half was discarded (valid curve point).
    pub(crate) const DEV_ROOT_KEY: &str = "sn/b0D2mU+3RfBglb8/Jy/2BfTWHvE2SyIgXkx3m8U8=";

    fn base() -> HashMap<&'static str, String> {
        HashMap::from([
            ("VGAMES_PUBLIC_URL", "http://localhost:8080".to_string()),
            (
                "VGAMES_SERVER_ID",
                "01920000-0000-7000-8000-000000000000".to_string(),
            ),
            ("DATABASE_URL", "postgres://u:p@localhost/db".to_string()),
            ("VGAMES_ROOT_PUBLIC_KEY", DEV_ROOT_KEY.to_string()),
            ("VGAMES_SERVER_SECRET", "a".repeat(44)),
            ("DISCORD_CLIENT_ID", "123456789012".to_string()),
            ("DISCORD_CLIENT_SECRET", "s3cret".to_string()),
            (
                "DISCORD_REDIRECT_URI",
                "http://localhost:8080/v1/auth/discord/callback".to_string(),
            ),
            ("VGAMES_STORAGE_BACKEND", "fs".to_string()),
            ("VGAMES_FS_STORAGE_ROOT", "/tmp/vgames".to_string()),
            ("VGAMES_FS_URL_SIGNING_KEY", "b".repeat(44)),
        ])
    }

    fn load(m: &HashMap<&'static str, String>) -> Result<Config, ConfigErrors> {
        Config::from_lookup(|k| m.get(k).cloned())
    }

    #[test]
    fn valid_minimal_config_uses_defaults() {
        let c = load(&base()).expect("valid");
        assert_eq!(c.bind_addr.to_string(), "127.0.0.1:8080");
        assert_eq!(c.log_format, LogFormat::Pretty);
        assert_eq!(c.db_max_connections, 20);
        assert_eq!(c.signed_url_ttl, Duration::from_secs(21600));
        assert!(matches!(c.discord, DiscordConfig::Discord(_)));
        assert!(matches!(c.storage, StorageConfig::Fs { .. }));
        assert_eq!(c.database_migration_url.expose(), c.database_url.expose());
        assert!(!c.is_https());
    }

    #[test]
    fn empty_environment_reports_every_required_variable() {
        let err = Config::from_lookup(|_| None).expect_err("must fail");
        for key in [
            "VGAMES_PUBLIC_URL",
            "VGAMES_SERVER_ID",
            "DATABASE_URL",
            "VGAMES_ROOT_PUBLIC_KEY",
            "VGAMES_SERVER_SECRET",
            "DISCORD_CLIENT_ID",
            "DISCORD_CLIENT_SECRET",
            "DISCORD_REDIRECT_URI",
            "VGAMES_STORAGE_BACKEND",
        ] {
            assert!(
                err.0.iter().any(|e| e.starts_with(key)),
                "missing report for {key}: {err}"
            );
        }
    }

    #[test]
    fn invalid_values_are_all_reported_together() {
        let mut m = base();
        m.insert("VGAMES_BIND_ADDR", "not-an-addr".into());
        m.insert("VGAMES_PUBLIC_URL", "http://example.com".into());
        m.insert("VGAMES_SERVER_ID", "nope".into());
        m.insert("VGAMES_ROOT_PUBLIC_KEY", "AAAA".into());
        m.insert("VGAMES_SERVER_SECRET", "c2hvcnQ=".into());
        m.insert("VGAMES_LOG_FORMAT", "xml".into());
        m.insert("VGAMES_STORAGE_BACKEND", "s3".into());
        m.insert("IGDB_CLIENT_ID", "only-one".into());
        let err = load(&m).expect_err("must fail");
        assert_eq!(err.0.len(), 8, "{err}");
        let text = err.to_string();
        assert!(
            text.contains("VGAMES_PUBLIC_URL is invalid: must use https"),
            "{text}"
        );
        assert!(
            text.contains("VGAMES_ROOT_PUBLIC_KEY is invalid: must decode to 32 bytes"),
            "{text}"
        );
    }

    #[test]
    fn redirect_uri_must_match_public_url() {
        let mut m = base();
        m.insert(
            "DISCORD_REDIRECT_URI",
            "http://localhost:9999/v1/auth/discord/callback".into(),
        );
        let err = load(&m).expect_err("must fail");
        assert!(
            err.0[0].contains("must be exactly http://localhost:8080/v1/auth/discord/callback")
        );
    }

    #[test]
    fn root_key_must_be_a_curve_point() {
        assert!(decode_ed25519_public_key(DEV_ROOT_KEY).is_ok());
        // About half of all 32-byte strings do not decompress to a curve point; find one.
        let bad = (0u8..=255)
            .map(|b| [b; 32])
            .find(|k| ed25519_dalek::VerifyingKey::from_bytes(k).is_err())
            .expect("some byte pattern is not a point");
        let b64 = base64::engine::general_purpose::STANDARD.encode(bad);
        assert!(
            decode_ed25519_public_key(&b64)
                .unwrap_err()
                .contains("not a valid Ed25519")
        );
        // The identity point decodes but has small order: useless as a signing key.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        let b64 = base64::engine::general_purpose::STANDARD.encode(identity);
        assert!(decode_ed25519_public_key(&b64).is_err());
    }

    #[test]
    fn fake_discord_needs_localhost() {
        let mut m = base();
        m.remove("DISCORD_CLIENT_ID");
        m.remove("DISCORD_CLIENT_SECRET");
        m.remove("DISCORD_REDIRECT_URI");
        m.insert("VGAMES_DEV_FAKE_DISCORD", "1".into());
        assert!(matches!(
            load(&m).expect("valid").discord,
            DiscordConfig::Fake
        ));
        m.insert("VGAMES_PUBLIC_URL", "https://vgames.example".into());
        let err = load(&m).expect_err("must fail");
        assert!(
            err.0
                .iter()
                .any(|e| e.contains("requires VGAMES_PUBLIC_URL to be localhost"))
        );
    }

    #[test]
    fn gcs_backend_requires_buckets() {
        let mut m = base();
        m.insert("VGAMES_STORAGE_BACKEND", "gcs".into());
        let err = load(&m).expect_err("must fail");
        assert_eq!(err.0.len(), 3, "{err}");
        m.insert("GCS_BUCKET_PACKAGES", "vg-packages".into());
        m.insert("GCS_BUCKET_SAVES", "vg-saves".into());
        m.insert("GCS_BUCKET_ASSETS", "Bad_Bucket".into());
        let err = load(&m).expect_err("must fail");
        assert!(err.0[0].starts_with("GCS_BUCKET_ASSETS is not a valid bucket name"));
    }
}
