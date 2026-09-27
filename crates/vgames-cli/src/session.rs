//! Signed-in sessions: tokens per server, kept in the OS keychain.
//!
//! - Stored per server origin in the OS keychain (service `vgames-cli`). Where no
//!   keychain is available (headless Linux), in a `0600` file in the user's
//!   config directory, with a warning (as the launcher does, 01-security §7).
//!   `VGAMES_CREDENTIAL_STORE=file` forces the file (CI, tests);
//!   `VGAMES_CONFIG_DIR` moves it.
//! - The server's root key is pinned at `vgames login` (fingerprint shown for
//!   comparison, or `--fingerprint`) and moves only along `next_root` rotations
//!   seen in verified trust bundles.
//! - Refresh tokens rotate on every use, so a refreshed pair is stored before it
//!   is used. `VGAMES_ACCESS_TOKEN` (one short-lived access token, never stored)
//!   is the non-interactive alternative for scripts.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;
use vgames_core::sign::PublicKey;
use vgames_core::trust::RootPin;
use vgames_proto::auth::{Role, TokenRequest, TokenResponse};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::server::{Api, ServerArgs, problem_code};

const SERVICE: &str = "vgames-cli";
const FORMAT: &str = "vgames.cli-session/1";
/// Refresh when the access token has less than this left.
const REFRESH_MARGIN_SECS: i64 = 60;

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    format: String,
    pub origin: String,
    #[zeroize(skip)]
    pub server_id: Uuid,
    /// Root public key (base64) pinned at login, and the announced next root.
    pub root: String,
    pub next_root: Option<String>,
    pub user_id: String,
    pub username: String,
    #[zeroize(skip)]
    pub role: Role,
    access_token: String,
    /// Unix seconds.
    #[zeroize(skip)]
    access_expires_at: i64,
    refresh_token: String,
}

impl Credentials {
    pub fn new(
        origin: &Url,
        server_id: Uuid,
        pin: &RootPin,
        tokens: &TokenResponse,
    ) -> Result<Self> {
        check_token("access", "vga_", &tokens.access_token)?;
        check_token("refresh", "vgr_", &tokens.refresh_token)?;
        Ok(Self {
            format: FORMAT.to_owned(),
            origin: origin.to_string(),
            server_id,
            root: pin.root.to_base64(),
            next_root: pin.next_root.as_ref().map(PublicKey::to_base64),
            user_id: tokens.user.id.to_string(),
            username: tokens.user.username.clone(),
            role: tokens.user.role,
            access_token: tokens.access_token.clone(),
            access_expires_at: now().saturating_add(tokens.expires_in.clamp(0, 86_400)),
            refresh_token: tokens.refresh_token.clone(),
        })
    }

    pub fn pin(&self) -> Result<RootPin> {
        Ok(RootPin {
            root: PublicKey::from_base64(&self.root).context("stored root key")?,
            next_root: self
                .next_root
                .as_deref()
                .map(PublicKey::from_base64)
                .transpose()
                .context("stored next root key")?,
        })
    }

    pub fn set_pin(&mut self, pin: &RootPin) {
        self.root = pin.root.to_base64();
        self.next_root = pin.next_root.as_ref().map(PublicKey::to_base64);
    }

    fn rotate(&mut self, tokens: &TokenResponse) -> Result<()> {
        check_token("access", "vga_", &tokens.access_token)?;
        check_token("refresh", "vgr_", &tokens.refresh_token)?;
        self.access_token = tokens.access_token.clone();
        self.refresh_token = tokens.refresh_token.clone();
        self.access_expires_at = now().saturating_add(tokens.expires_in.clamp(0, 86_400));
        self.role = tokens.user.role;
        Ok(())
    }
}

fn check_token(what: &str, prefix: &str, token: &str) -> Result<()> {
    let body = token.strip_prefix(prefix).unwrap_or_default();
    if body.len() != 43
        || !body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("the server returned a malformed {what} token");
    }
    Ok(())
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

// ---------------------------------------------------------------------------
// Storage

enum Store {
    Keychain,
    File(PathBuf),
}

pub(crate) fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("VGAMES_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    directories::ProjectDirs::from("app", "vgames", "vgames-cli")
        .map(|d| d.config_dir().to_path_buf())
        .context("no home directory to store credentials in (set VGAMES_CONFIG_DIR)")
}

fn file_for(origin: &str) -> Result<PathBuf> {
    let digest = vgames_core::Digest::of(origin.as_bytes()).to_string();
    let name = format!("session-{}.json", digest.get(..16).unwrap_or(&digest));
    Ok(config_dir()?.join(name))
}

fn force_file() -> bool {
    std::env::var("VGAMES_CREDENTIAL_STORE").is_ok_and(|v| v == "file")
}

fn keychain(origin: &str) -> Option<keyring::Entry> {
    if force_file() {
        return None;
    }
    keyring::Entry::new(SERVICE, origin).ok()
}

pub(crate) fn write_private(path: &PathBuf, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write as _;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut f = options
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Saves `creds`; returns where they went.
fn save(creds: &Credentials) -> Result<Store> {
    let json = Zeroizing::new(serde_json::to_string(creds).context("encoding the session")?);
    if let Some(entry) = keychain(&creds.origin) {
        match entry.set_password(&json) {
            Ok(()) => {
                // A stale file from an earlier keychain-less run must not linger.
                if let Ok(path) = file_for(&creds.origin) {
                    let _ = std::fs::remove_file(path);
                }
                return Ok(Store::Keychain);
            }
            Err(e) => tracing::debug!(error = %e, "keychain unavailable"),
        }
    }
    let path = file_for(&creds.origin)?;
    write_private(&path, json.as_bytes())?;
    Ok(Store::File(path))
}

pub fn load(origin: &Url) -> Result<Option<Credentials>> {
    let key = origin.to_string();
    let text: Option<Zeroizing<String>> = match keychain(&key).map(|e| e.get_password()) {
        Some(Ok(t)) => Some(Zeroizing::new(t)),
        _ => {
            let path = file_for(&key)?;
            match std::fs::read_to_string(&path) {
                Ok(t) => Some(Zeroizing::new(t)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
            }
        }
    };
    let Some(text) = text else { return Ok(None) };
    let creds: Credentials =
        serde_json::from_str(&text).context("the stored session is unreadable; sign in again")?;
    if creds.format != FORMAT || creds.origin != key {
        bail!("the stored session is for another server or format; sign in again");
    }
    Ok(Some(creds))
}

pub fn delete(origin: &Url) -> Result<()> {
    let key = origin.to_string();
    if let Some(entry) = keychain(&key) {
        let _ = entry.delete_credential();
    }
    match std::fs::remove_file(file_for(&key)?) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Saves and tells the user where the tokens are.
pub fn save_and_report(creds: &Credentials) -> Result<()> {
    match save(creds)? {
        Store::Keychain => eprintln!("Tokens are stored in the OS keychain."),
        Store::File(path) => eprintln!(
            "warning: no OS keychain is available; tokens are stored in {} (readable only by you).",
            path.display()
        ),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Signed-in API

/// A signed-in client for `server`, refreshing the tokens when needed.
pub struct Session {
    pub api: Api,
    /// `None` with `VGAMES_ACCESS_TOKEN`.
    pub creds: Option<Credentials>,
}

impl Session {
    pub async fn open(server: &ServerArgs) -> Result<Self> {
        let origin = server.origin()?;
        if let Ok(token) = std::env::var("VGAMES_ACCESS_TOKEN") {
            check_token("access", "vga_", &token)
                .context("VGAMES_ACCESS_TOKEN is not an access token")?;
            let api = Api::new(origin)?.with_token(Zeroizing::new(token));
            return Ok(Self { api, creds: None });
        }
        let Some(mut creds) = load(&origin)? else {
            bail!("not signed in to {origin}; run `vgames login --server {origin}` first");
        };
        if creds.access_expires_at - now() < REFRESH_MARGIN_SECS {
            refresh(&origin, &mut creds).await?;
        }
        let api = Api::new(origin)?.with_token(Zeroizing::new(creds.access_token.clone()));
        Ok(Self {
            api,
            creds: Some(creds),
        })
    }

    /// Stores a moved root pin after a verified trust bundle.
    pub fn update_pin(&mut self, pin: &RootPin) -> Result<()> {
        if let Some(creds) = &mut self.creds
            && creds.pin()? != *pin
        {
            creds.set_pin(pin);
            save(creds)?;
            eprintln!(
                "The server's root key rotated; now pinned to {}.",
                pin.root.fingerprint()
            );
        }
        Ok(())
    }
}

async fn refresh(origin: &Url, creds: &mut Credentials) -> Result<()> {
    let api = Api::new(origin.clone())?;
    let request = TokenRequest::RefreshToken {
        refresh_token: creds.refresh_token.clone(),
    };
    let tokens: TokenResponse = match api.post("/v1/auth/token", &request).await {
        Ok(t) => t,
        Err(e) => {
            if let Some((401, code)) = problem_code(&e) {
                delete(origin)?;
                bail!(
                    "the session on {origin} ended ({code}); run `vgames login --server {origin}` again"
                );
            }
            return Err(e);
        }
    };
    creds.rotate(&tokens)?;
    // The old refresh token is now spent: persist the new pair before anything else.
    save(creds)?;
    Ok(())
}
