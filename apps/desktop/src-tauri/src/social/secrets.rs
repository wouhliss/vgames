//! 32-byte secrets for the social module (05-social §4.3, 01-security §7): the Olm pickle
//! key and the chat-database key live in the OS keychain. Without a keychain (headless
//! Linux), they live in `0600` files in the app data dir and Settings shows a warning
//! ([`SecretStore::is_fallback`]).
//!
//! Keychain calls can block (Secret Service over D-Bus): call these from a blocking task.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::crypto::SecretKey32;

/// Pickle key for Olm account and session pickles.
pub const PICKLE_KEY: &str = "social-olm-pickle-key";
/// Key for message bodies and the outbox in SQLite.
pub const CHAT_KEY: &str = "social-chat-db-key";

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("the OS keychain refused the request")]
    Keychain(#[source] keyring::Error),
    #[error("the stored secret is malformed")]
    Malformed,
    #[error("cannot read or write the secret file")]
    Io(#[source] std::io::Error),
    #[error("the OS random generator failed")]
    Random,
}

pub trait SecretStore: Send + Sync {
    /// Returns the named secret, creating and storing a random one on first use.
    fn get_or_create(&self, name: &str) -> Result<SecretKey32, SecretError>;
    /// `true` when secrets are kept in files because no keychain is available.
    fn is_fallback(&self) -> bool;
}

/// The OS keychain (Windows Credential Manager, macOS Keychain, Secret Service).
pub struct KeychainStore {
    service: String,
}

impl KeychainStore {
    /// `service` is the app identifier (profile-specific, so debug profiles do not collide).
    pub fn new(service: &str) -> Self {
        Self {
            service: service.to_owned(),
        }
    }

    fn entry(&self, name: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, name).map_err(SecretError::Keychain)
    }

    /// Whether the keychain answers at all (a missing entry counts as working).
    pub fn probe(&self) -> bool {
        match self.entry("social-probe").and_then(|e| {
            e.get_secret().map(|_| ()).or_else(|err| match err {
                keyring::Error::NoEntry => Ok(()),
                other => Err(SecretError::Keychain(other)),
            })
        }) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "OS keychain unavailable");
                false
            }
        }
    }
}

impl SecretStore for KeychainStore {
    fn get_or_create(&self, name: &str) -> Result<SecretKey32, SecretError> {
        let entry = self.entry(name)?;
        match entry.get_secret() {
            Ok(bytes) => {
                let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| SecretError::Malformed)?;
                Ok(SecretKey32::from_bytes(arr))
            }
            Err(keyring::Error::NoEntry) => {
                let key = SecretKey32::generate().map_err(|_| SecretError::Random)?;
                entry
                    .set_secret(key.expose())
                    .map_err(SecretError::Keychain)?;
                Ok(key)
            }
            Err(other) => Err(SecretError::Keychain(other)),
        }
    }

    fn is_fallback(&self) -> bool {
        false
    }
}

/// `0600` files under `<data dir>/secrets/` (fallback when there is no keychain).
pub struct FileStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl FileStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
            lock: Mutex::new(()),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.key"))
    }
}

impl SecretStore for FileStore {
    fn get_or_create(&self, name: &str) -> Result<SecretKey32, SecretError> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = self.path(name);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| SecretError::Malformed)?;
                return Ok(SecretKey32::from_bytes(arr));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(SecretError::Io(e)),
        }
        std::fs::create_dir_all(&self.dir).map_err(SecretError::Io)?;
        let key = SecretKey32::generate().map_err(|_| SecretError::Random)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(SecretError::Io)?;
        file.write_all(key.expose()).map_err(SecretError::Io)?;
        file.sync_all().map_err(SecretError::Io)?;
        Ok(key)
    }

    fn is_fallback(&self) -> bool {
        true
    }
}

/// In-memory secrets (tests).
#[derive(Default)]
pub struct MemoryStore {
    keys: Mutex<std::collections::HashMap<String, SecretKey32>>,
}

impl SecretStore for MemoryStore {
    fn get_or_create(&self, name: &str) -> Result<SecretKey32, SecretError> {
        let mut keys = self
            .keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(k) = keys.get(name) {
            return Ok(k.clone());
        }
        let k = SecretKey32::generate().map_err(|_| SecretError::Random)?;
        keys.insert(name.to_owned(), k.clone());
        Ok(k)
    }

    fn is_fallback(&self) -> bool {
        false
    }
}

/// The keychain when it works, else the file fallback (logged; Settings warns).
pub fn open(service: &str, data_dir: &Path) -> Box<dyn SecretStore> {
    let keychain = KeychainStore::new(service);
    if keychain.probe() {
        Box::new(keychain)
    } else {
        tracing::warn!("storing social secrets in 0600 files: no OS keychain");
        Box::new(FileStore::new(&data_dir.join("secrets")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_creates_once_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(&dir.path().join("secrets"));
        let a = store.get_or_create(PICKLE_KEY).unwrap();
        let b = store.get_or_create(PICKLE_KEY).unwrap();
        assert_eq!(a.expose(), b.expose());
        let c = store.get_or_create(CHAT_KEY).unwrap();
        assert_ne!(a.expose(), c.expose());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join("secrets").join("social-olm-pickle-key.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(store.is_fallback());
        // A truncated file is reported, not silently replaced.
        std::fs::write(dir.path().join("secrets").join("broken.key"), b"short").unwrap();
        assert!(matches!(store.get_or_create("broken"), Err(SecretError::Malformed)));
    }

    #[test]
    fn memory_store_is_stable() {
        let store = MemoryStore::default();
        assert_eq!(
            store.get_or_create("x").unwrap().expose(),
            store.get_or_create("x").unwrap().expose()
        );
    }
}
