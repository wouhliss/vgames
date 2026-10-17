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
    #[error("the secret file is a symlink, not a regular file, or readable by other users")]
    Insecure,
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
                let arr: [u8; 32] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| SecretError::Malformed)?;
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
///
/// A key file is created once and never overwritten: it is written to a uniquely named
/// temp file in the same directory, synced, then linked into place, so a crash can never
/// leave a partial key file behind (stale temp files are removed on the next creation).
/// A key file that is a symlink or not a regular file is refused ([`SecretError::Insecure`]);
/// on unix so is one readable or writable by group or others. On Windows there are no mode
/// bits: the files rely on the per-user ACL of the app data directory (`%APPDATA%`), which
/// only the user can read.
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

    /// Prefix of the temp files for `name` (`.<name>.key.tmp-<random>`).
    fn temp_prefix(name: &str) -> String {
        format!(".{name}.key.tmp-")
    }

    /// Reads an existing key file; `Ok(None)` when there is none.
    fn read_existing(path: &Path) -> Result<Option<SecretKey32>, SecretError> {
        let meta = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(SecretError::Io(e)),
        };
        check_file(&meta)?;
        let mut file = std::fs::File::open(path).map_err(SecretError::Io)?;
        // The opened file must be the one checked above (no swap in between).
        let opened = file.metadata().map_err(SecretError::Io)?;
        check_file(&opened)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if (opened.dev(), opened.ino()) != (meta.dev(), meta.ino()) {
                return Err(SecretError::Insecure);
            }
        }
        let mut bytes = Vec::with_capacity(32);
        std::io::Read::read_to_end(&mut std::io::Read::take(&mut file, 33), &mut bytes)
            .map_err(SecretError::Io)?;
        let arr: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| SecretError::Malformed)?;
        Ok(Some(SecretKey32::from_bytes(arr)))
    }

    /// Best effort: removes temp files a crash left behind for `name`.
    fn remove_stale_temps(&self, name: &str) {
        let prefix = Self::temp_prefix(name);
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix)
                && let Err(error) = std::fs::remove_file(entry.path())
            {
                tracing::warn!(%error, "cannot remove a stale secret temp file");
            }
        }
    }

    /// Creates a new uniquely named private temp file in the secrets directory.
    fn create_temp(&self, name: &str) -> Result<(PathBuf, std::fs::File), SecretError> {
        let mut last_error = None;
        for _ in 0..8 {
            let mut suffix = [0u8; 8];
            getrandom::fill(&mut suffix).map_err(|_| SecretError::Random)?;
            let suffix: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
            let tmp = self
                .dir
                .join(format!("{}{suffix}", Self::temp_prefix(name)));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            match options.open(&tmp) {
                Ok(file) => return Ok((tmp, file)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last_error = Some(e),
                Err(e) => return Err(SecretError::Io(e)),
            }
        }
        Err(SecretError::Io(last_error.unwrap_or_else(|| {
            std::io::Error::from(std::io::ErrorKind::AlreadyExists)
        })))
    }

    /// Writes `key` to a temp file and moves it to `path` without overwriting an existing
    /// file. Returns `false` when `path` appeared meanwhile (another process won the race).
    fn write_new(&self, name: &str, path: &Path, key: &SecretKey32) -> Result<bool, SecretError> {
        let (tmp, mut file) = self.create_temp(name)?;
        let result = (|| {
            file.write_all(key.expose()).map_err(SecretError::Io)?;
            file.sync_all().map_err(SecretError::Io)?;
            drop(file);
            // `hard_link` fails if the target exists: the "create once" rule holds even
            // across processes. Filesystems without hard links fall back to `rename`.
            match std::fs::hard_link(&tmp, path) {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                Err(_) => std::fs::rename(&tmp, path)
                    .map(|()| true)
                    .map_err(SecretError::Io),
            }
        })();
        if let Err(error) = std::fs::remove_file(&tmp)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "cannot remove a secret temp file");
        }
        let created = result?;
        #[cfg(unix)]
        if created {
            // Persist the new directory entry.
            std::fs::File::open(&self.dir)
                .and_then(|dir| dir.sync_all())
                .map_err(SecretError::Io)?;
        }
        Ok(created)
    }

    fn create_dir(&self) -> Result<(), SecretError> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(&self.dir).map_err(SecretError::Io)
    }
}

/// Refuses anything but a private regular file.
fn check_file(meta: &std::fs::Metadata) -> Result<(), SecretError> {
    if !meta.file_type().is_file() {
        return Err(SecretError::Insecure);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(SecretError::Insecure);
        }
    }
    Ok(())
}

impl SecretStore for FileStore {
    fn get_or_create(&self, name: &str) -> Result<SecretKey32, SecretError> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = self.path(name);
        if let Some(key) = Self::read_existing(&path)? {
            return Ok(key);
        }
        self.create_dir()?;
        self.remove_stale_temps(name);
        let key = SecretKey32::generate().map_err(|_| SecretError::Random)?;
        if self.write_new(name, &path, &key)? {
            Ok(key)
        } else {
            Self::read_existing(&path)?.ok_or(SecretError::Malformed)
        }
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
            let mode =
                std::fs::metadata(dir.path().join("secrets").join("social-olm-pickle-key.key"))
                    .unwrap()
                    .permissions()
                    .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(store.is_fallback());
        // A truncated file is reported, not silently replaced.
        let broken = dir.path().join("secrets").join("broken.key");
        std::fs::write(&broken, b"short").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&broken, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(matches!(
            store.get_or_create("broken"),
            Err(SecretError::Malformed)
        ));
    }

    #[test]
    fn file_store_ignores_and_removes_a_partial_temp_file_from_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        let stale = secrets.join(".social-chat-db-key.key.tmp-deadbeef");
        std::fs::write(&stale, b"part").unwrap();
        let store = FileStore::new(&secrets);
        let a = store.get_or_create(CHAT_KEY).unwrap();
        let b = store.get_or_create(CHAT_KEY).unwrap();
        assert_eq!(a.expose(), b.expose());
        assert_eq!(
            std::fs::read(secrets.join("social-chat-db-key.key")).unwrap(),
            a.expose()
        );
        assert!(!stale.exists(), "the stale temp file is cleaned up");
        let leftovers: Vec<_> = std::fs::read_dir(&secrets)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn file_store_refuses_a_key_file_readable_by_others() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path());
        let path = dir.path().join("social-olm-pickle-key.key");
        std::fs::write(&path, [7u8; 32]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            store.get_or_create(PICKLE_KEY),
            Err(SecretError::Insecure)
        ));
        // Refused, not replaced.
        assert_eq!(std::fs::read(&path).unwrap(), [7u8; 32]);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            store.get_or_create(PICKLE_KEY).unwrap().expose(),
            &[7u8; 32]
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_store_refuses_a_symlinked_key_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere.bin");
        std::fs::write(&elsewhere, [9u8; 32]).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o600)).unwrap();
        let secrets = dir.path().join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        std::os::unix::fs::symlink(&elsewhere, secrets.join("social-chat-db-key.key")).unwrap();
        let store = FileStore::new(&secrets);
        assert!(matches!(
            store.get_or_create(CHAT_KEY),
            Err(SecretError::Insecure)
        ));
        // A dangling symlink is refused too, not written through.
        std::fs::remove_file(&elsewhere).unwrap();
        assert!(matches!(
            store.get_or_create(CHAT_KEY),
            Err(SecretError::Insecure)
        ));
        assert!(!elsewhere.exists());
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
