//! Where session tokens live at rest (01-security §4.2, §7): the OS keychain
//! (Windows Credential Manager, macOS Keychain, Secret Service). When no
//! keychain answers (headless Linux), a `0600` file per entry in the app data
//! dir, and the UI shows a persistent warning ([`TokenVault::is_fallback`]).
//!
//! Values are small strings (a JSON token pair). Keychain calls can block
//! (Secret Service goes over D-Bus), so async code calls these through
//! `spawn_blocking` (see [`VaultHandle`]).

use std::collections::HashMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("the OS keychain refused the request")]
    Keychain(#[source] keyring::Error),
    #[error("cannot read or write the token file")]
    Io(#[source] std::io::Error),
    #[error("the stored value is not valid UTF-8")]
    Malformed,
    #[error("the token store task stopped")]
    Task,
}

/// A small key/value store for secrets. Keys are `[a-z0-9:-]` names.
pub trait TokenVault: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<Zeroizing<String>>, VaultError>;
    fn set(&self, key: &str, value: &str) -> Result<(), VaultError>;
    /// Deleting a missing entry is not an error.
    fn delete(&self, key: &str) -> Result<(), VaultError>;
    /// `true` when secrets are kept in files because no keychain is available.
    fn is_fallback(&self) -> bool;
}

/// The OS keychain.
pub struct KeychainVault {
    service: String,
}

impl KeychainVault {
    /// `service` is the app identifier (profile-specific, so debug profiles do not collide).
    pub fn new(service: &str) -> Self {
        Self {
            service: service.to_owned(),
        }
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry, VaultError> {
        keyring::Entry::new(&self.service, key).map_err(VaultError::Keychain)
    }

    /// Whether the keychain answers at all (a missing entry counts as working).
    pub fn probe(&self) -> bool {
        match self.get("probe") {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(%error, "OS keychain unavailable for session tokens");
                false
            }
        }
    }
}

impl TokenVault for KeychainVault {
    fn get(&self, key: &str) -> Result<Option<Zeroizing<String>>, VaultError> {
        match self.entry(key)?.get_password() {
            Ok(value) => Ok(Some(Zeroizing::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(VaultError::Keychain(error)),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), VaultError> {
        self.entry(key)?
            .set_password(value)
            .map_err(VaultError::Keychain)
    }

    fn delete(&self, key: &str) -> Result<(), VaultError> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(VaultError::Keychain(error)),
        }
    }

    fn is_fallback(&self) -> bool {
        false
    }
}

/// One `0600` file per key under `dir` (created `0700`).
pub struct FileVault {
    dir: PathBuf,
}

impl FileVault {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, key: &str) -> PathBuf {
        // Keys are fixed names chosen by this crate; map ':' so the name is a
        // valid file name everywhere.
        self.dir.join(format!("{}.secret", key.replace(':', "_")))
    }

    fn ensure_dir(&self) -> Result<(), VaultError> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&self.dir).map_err(VaultError::Io)
    }
}

impl TokenVault for FileVault {
    fn get(&self, key: &str) -> Result<Option<Zeroizing<String>>, VaultError> {
        match std::fs::read(self.path(key)) {
            Ok(bytes) => String::from_utf8(bytes)
                .map(|s| Some(Zeroizing::new(s)))
                .map_err(|_| VaultError::Malformed),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(VaultError::Io(error)),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), VaultError> {
        self.ensure_dir()?;
        let path = self.path(key);
        let tmp = path.with_extension("secret.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp).map_err(VaultError::Io)?;
        file.write_all(value.as_bytes()).map_err(VaultError::Io)?;
        file.sync_all().map_err(VaultError::Io)?;
        drop(file);
        std::fs::rename(&tmp, &path).map_err(VaultError::Io)
    }

    fn delete(&self, key: &str) -> Result<(), VaultError> {
        match std::fs::remove_file(self.path(key)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(VaultError::Io(error)),
        }
    }

    fn is_fallback(&self) -> bool {
        true
    }
}

/// In-memory store for tests.
#[derive(Default)]
pub struct MemoryVault {
    entries: Mutex<HashMap<String, String>>,
}

impl MemoryVault {
    pub fn len(&self) -> usize {
        self.entries.lock().map(|e| e.len()).unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl TokenVault for MemoryVault {
    fn get(&self, key: &str) -> Result<Option<Zeroizing<String>>, VaultError> {
        let entries = self.entries.lock().map_err(|_| VaultError::Task)?;
        Ok(entries.get(key).map(|v| Zeroizing::new(v.clone())))
    }

    fn set(&self, key: &str, value: &str) -> Result<(), VaultError> {
        let mut entries = self.entries.lock().map_err(|_| VaultError::Task)?;
        entries.insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), VaultError> {
        let mut entries = self.entries.lock().map_err(|_| VaultError::Task)?;
        entries.remove(key);
        Ok(())
    }

    fn is_fallback(&self) -> bool {
        false
    }
}

/// Picks the keychain when it answers, else the file store under `fallback_dir`.
pub fn open_default(service: &str, fallback_dir: PathBuf) -> Arc<dyn TokenVault> {
    let keychain = KeychainVault::new(service);
    if keychain.probe() {
        Arc::new(keychain)
    } else {
        tracing::warn!("storing session tokens in a private file (no OS keychain)");
        Arc::new(FileVault::new(fallback_dir))
    }
}

/// Async wrapper that runs vault calls on the blocking pool.
#[derive(Clone)]
pub struct VaultHandle(pub Arc<dyn TokenVault>);

impl VaultHandle {
    pub async fn get(&self, key: String) -> Result<Option<Zeroizing<String>>, VaultError> {
        let vault = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || vault.get(&key))
            .await
            .map_err(|_| VaultError::Task)?
    }

    pub async fn set(&self, key: String, value: Zeroizing<String>) -> Result<(), VaultError> {
        let vault = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || vault.set(&key, &value))
            .await
            .map_err(|_| VaultError::Task)?
    }

    pub async fn delete(&self, key: String) -> Result<(), VaultError> {
        let vault = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || vault.delete(&key))
            .await
            .map_err(|_| VaultError::Task)?
    }

    pub fn is_fallback(&self) -> bool {
        self.0.is_fallback()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_vault_round_trips_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let vault = FileVault::new(dir.path().join("secrets"));
        assert!(vault.get("tokens:a").unwrap().is_none());
        vault.set("tokens:a", "one").unwrap();
        vault.set("tokens:a", "two").unwrap();
        assert_eq!(vault.get("tokens:a").unwrap().unwrap().as_str(), "two");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = std::fs::metadata(vault.path("tokens:a")).unwrap();
            assert_eq!(file.permissions().mode() & 0o777, 0o600);
            let dir = std::fs::metadata(dir.path().join("secrets")).unwrap();
            assert_eq!(dir.permissions().mode() & 0o777, 0o700);
        }
        vault.delete("tokens:a").unwrap();
        vault.delete("tokens:a").unwrap();
        assert!(vault.get("tokens:a").unwrap().is_none());
        assert!(vault.is_fallback());
    }
}
