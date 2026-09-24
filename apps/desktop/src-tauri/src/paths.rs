//! Where the launcher keeps its own files, and the debug-only `VGAMES_PROFILE`
//! switch that gives a second launcher on the same machine its own data dir,
//! logs, WebView storage and single-instance lock (milestone M3).

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager, Runtime};

/// Environment variable naming a debug profile (`alice`, `bob`, …).
pub const PROFILE_ENV: &str = "VGAMES_PROFILE";

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("{PROFILE_ENV} must be 1-32 characters of a-z, 0-9 or '-', got {0:?}")]
    InvalidProfile(String),
    #[error("cannot resolve the {what} directory")]
    Resolve {
        what: &'static str,
        #[source]
        source: tauri::Error,
    },
    #[error("cannot create {path}")]
    Create {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Reads `VGAMES_PROFILE`. Debug builds only: release builds ignore it so a
/// stray variable cannot split a player's data.
pub fn debug_profile() -> Result<Option<String>, PathsError> {
    let Some(raw) = std::env::var_os(PROFILE_ENV) else {
        return Ok(None);
    };
    if !cfg!(debug_assertions) {
        return Ok(None);
    }
    let value = raw.to_string_lossy().into_owned();
    if is_valid_profile(&value) {
        Ok(Some(value))
    } else {
        Err(PathsError::InvalidProfile(value))
    }
}

fn is_valid_profile(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The bundle identifier used for a profile. Tauri derives the app data, log
/// and WebView directories and the single-instance lock from it.
pub fn profile_identifier(base: &str, profile: Option<&str>) -> String {
    match profile {
        Some(profile) => format!("{base}.profile-{profile}"),
        None => base.to_owned(),
    }
}

/// Launcher-owned directories, created at startup.
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub profile: Option<String>,
    /// SQLite database, runtimes, prefixes, save backups, …
    pub data_dir: PathBuf,
    /// Rolling logs and crash reports.
    pub log_dir: PathBuf,
    /// Disposable caches (images).
    pub cache_dir: PathBuf,
}

impl AppPaths {
    pub fn resolve<R: Runtime>(
        app: &AppHandle<R>,
        profile: Option<String>,
    ) -> Result<Self, PathsError> {
        let resolver = app.path();
        let data_dir = resolver
            .app_data_dir()
            .map_err(|source| PathsError::Resolve {
                what: "app data",
                source,
            })?;
        let log_dir = resolver
            .app_log_dir()
            .map_err(|source| PathsError::Resolve {
                what: "log",
                source,
            })?;
        let cache_dir = resolver
            .app_cache_dir()
            .map_err(|source| PathsError::Resolve {
                what: "cache",
                source,
            })?;
        let paths = Self {
            profile,
            data_dir,
            log_dir,
            cache_dir,
        };
        for dir in [&paths.data_dir, &paths.log_dir, &paths.cache_dir] {
            create_private_dir(dir)?;
        }
        Ok(paths)
    }

    pub fn database_file(&self) -> PathBuf {
        self.data_dir.join("vgames.sqlite3")
    }
}

/// Creates a directory readable only by the current user where the OS supports it.
pub fn create_private_dir(path: &Path) -> Result<(), PathsError> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|source| PathsError::Create {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_names() {
        assert!(is_valid_profile("alice"));
        assert!(is_valid_profile("bob-2"));
        assert!(!is_valid_profile(""));
        assert!(!is_valid_profile("Alice"));
        assert!(!is_valid_profile("../x"));
        assert!(!is_valid_profile("a b"));
        assert!(!is_valid_profile(&"a".repeat(33)));
    }

    #[test]
    fn identifiers() {
        assert_eq!(
            profile_identifier("app.vgames.launcher", None),
            "app.vgames.launcher"
        );
        assert_eq!(
            profile_identifier("app.vgames.launcher", Some("alice")),
            "app.vgames.launcher.profile-alice"
        );
    }
}
