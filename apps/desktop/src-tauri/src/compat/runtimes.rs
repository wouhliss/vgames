//! Installed compatibility runtimes (09-compatibility §5, GAME-03).
//!
//! A runtime is used only if the catalog naming it verified (`compat::catalog`)
//! and its archive matched that entry: the size is enforced while downloading, the
//! SHA-256 is checked **before** anything is extracted, and extraction follows
//! `compat::extract`'s path rules into a staging directory that is renamed to
//! `<data_dir>/runtimes/<id>/<version>/` only when complete.
//!
//! Downloads resume from a `.part` file (HTTP range requests). Each installed
//! package records the runtime versions it uses; [`RuntimeStore::collect_garbage`]
//! removes every version no installed package references, and
//! [`RuntimeStore::remove`] refuses a version still in use.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use specta::Type;
use tokio::io::AsyncWriteExt as _;
use url::Url;
use vgames_core::runtimes::{Runtime, RuntimeId};

use super::extract::{self, ExtractError};
use crate::db::{Db, DbError, now_unix};
use crate::events::PackageRef;

/// A body chunk must arrive within this.
pub const READ_TIMEOUT: Duration = Duration::from_secs(60);
pub const MAX_REDIRECTS: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("the runtime could not be downloaded: {0}")]
    Network(String),
    #[error("the runtime download is not the size the catalog pins")]
    SizeMismatch,
    #[error("the runtime download does not match the catalog's SHA-256")]
    HashMismatch,
    #[error(transparent)]
    Extract(#[from] ExtractError),
    #[error("the runtime is still used by {used_by} installed package(s)")]
    InUse { used_by: u32 },
    #[error("cannot write the runtime: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// One extracted runtime version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
pub struct InstalledRuntime {
    pub runtime_id: String,
    pub version: String,
    pub size: u64,
    /// Installed packages that use it.
    pub used_by: u32,
}

pub struct RuntimeStore {
    root: PathBuf,
    db: Db,
    http: reqwest::Client,
    /// One install or collection at a time: GC never races an install.
    busy: tokio::sync::Mutex<()>,
}

impl RuntimeStore {
    /// `root` is `<data_dir>/runtimes`.
    pub fn new(root: PathBuf, db: Db, http: reqwest::Client) -> Self {
        Self {
            root,
            db,
            http,
            busy: tokio::sync::Mutex::new(()),
        }
    }

    /// The transfer stack's client: HTTP/1.1, no transparent decoding, no redirects
    /// (followed here, `https` only in release builds).
    pub fn http_client() -> reqwest::Result<reqwest::Client> {
        vgames_transfer::http::transfer_client(&vgames_transfer::http::ClientOptions::default())
    }

    pub fn dir(&self, id: RuntimeId, version: &str) -> PathBuf {
        self.root.join(id.as_str()).join(version)
    }

    /// The runtime's directory, downloading and extracting it first if needed, and
    /// recording that `package` uses it.
    pub async fn ensure(&self, rt: &Runtime, package: PackageRef) -> Result<PathBuf, RuntimeError> {
        let _busy = self.busy.lock().await;
        let dir = self.dir(rt.id, &rt.version);
        if !(is_recorded(&self.db, rt).await? && tokio::fs::try_exists(&dir).await?) {
            self.install(rt, &dir).await?;
        }
        add_ref(&self.db, package, rt).await?;
        Ok(dir)
    }

    async fn install(&self, rt: &Runtime, dir: &Path) -> Result<(), RuntimeError> {
        let work = self.root.join(".work");
        tokio::fs::create_dir_all(&work).await?;
        let part = work.join(format!("{}-{}.part", rt.id.as_str(), rt.version));
        self.download(rt, &part).await?;

        let (part_hash, expected) = (part.clone(), rt.sha256.clone());
        let matches = tokio::task::spawn_blocking(move || sha256_file(&part_hash))
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))??
            == expected;
        if !matches {
            let _ = tokio::fs::remove_file(&part).await;
            tracing::warn!(runtime = rt.id.as_str(), version = %rt.version, "runtime archive hash mismatch; nothing extracted");
            return Err(RuntimeError::HashMismatch);
        }

        let staging = work.join(format!(
            "{}-{}-{}",
            rt.id.as_str(),
            rt.version,
            uuid::Uuid::now_v7()
        ));
        tokio::fs::create_dir_all(&staging).await?;
        let (archive, format, into) = (part.clone(), rt.archive, staging.clone());
        let extracted =
            tokio::task::spawn_blocking(move || extract::extract(&archive, format, &into))
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        if let Err(error) = extracted {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            let _ = tokio::fs::remove_file(&part).await;
            return Err(error.into());
        }
        if tokio::fs::try_exists(dir).await? {
            remove_tree(dir.to_owned()).await?;
        }
        if let Some(parent) = dir.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::rename(&staging, dir).await?;
        let _ = tokio::fs::remove_file(&part).await;
        record(&self.db, rt).await?;
        tracing::info!(runtime = rt.id.as_str(), version = %rt.version, "runtime installed");
        Ok(())
    }

    /// Downloads into `part`, resuming what is there; never more than `rt.size` bytes.
    async fn download(&self, rt: &Runtime, part: &Path) -> Result<(), RuntimeError> {
        let mut have = match tokio::fs::metadata(part).await {
            Ok(m) => m.len(),
            Err(_) => 0,
        };
        if have > rt.size {
            tokio::fs::remove_file(part).await?;
            have = 0;
        }
        if have == rt.size {
            return Ok(());
        }
        let mut url = Url::parse(&rt.url).map_err(|e| RuntimeError::Network(e.to_string()))?;
        let mut hops = 0;
        let mut response = loop {
            let mut request = self.http.get(url.clone());
            if have > 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
            }
            let response = request
                .send()
                .await
                .map_err(|e| RuntimeError::Network(e.without_url().to_string()))?;
            if !response.status().is_redirection() {
                break response;
            }
            hops += 1;
            let next = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|l| url.join(l).ok())
                .ok_or_else(|| RuntimeError::Network("redirect without a location".into()))?;
            let allowed =
                next.scheme() == "https" || (cfg!(debug_assertions) && next.scheme() == "http");
            if hops > MAX_REDIRECTS || !allowed {
                return Err(RuntimeError::Network("refused redirect".into()));
            }
            url = next;
        };
        let status = response.status();
        let mut file = if status == reqwest::StatusCode::PARTIAL_CONTENT && have > 0 {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(part)
                .await?
        } else if status.is_success() {
            have = 0;
            tokio::fs::File::create(part).await?
        } else {
            return Err(RuntimeError::Network(format!("HTTP {}", status.as_u16())));
        };
        if response
            .content_length()
            .is_some_and(|len| have.saturating_add(len) > rt.size)
        {
            drop(file);
            let _ = tokio::fs::remove_file(part).await;
            return Err(RuntimeError::SizeMismatch);
        }
        loop {
            let chunk = tokio::time::timeout(READ_TIMEOUT, response.chunk())
                .await
                .map_err(|_| RuntimeError::Network("the download stalled".into()))?
                .map_err(|e| RuntimeError::Network(e.without_url().to_string()))?;
            let Some(chunk) = chunk else { break };
            have = have.saturating_add(chunk.len() as u64);
            if have > rt.size {
                drop(file);
                let _ = tokio::fs::remove_file(part).await;
                return Err(RuntimeError::SizeMismatch);
            }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        file.sync_all().await?;
        if have != rt.size {
            // A short body: keep the part for the next attempt.
            return Err(RuntimeError::Network("the download ended early".into()));
        }
        Ok(())
    }

    /// Forgets every reference of an uninstalled package (INS's after-uninstall hook).
    pub async fn package_removed(&self, package: PackageRef) -> Result<(), RuntimeError> {
        let (server, pkg) = (
            package.server_id.to_string(),
            package.package_id.to_string(),
        );
        self.db
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM compat_runtime_refs WHERE server_id = ?1 AND package_id = ?2",
                    params![server, pkg],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Drops references of packages that are not in `installed` (counted from
    /// `db::installs`), then removes every runtime version nothing references.
    /// Returns the removed (id, version) pairs.
    pub async fn collect_garbage(
        &self,
        installed: &HashSet<PackageRef>,
    ) -> Result<Vec<(String, String)>, RuntimeError> {
        let _busy = self.busy.lock().await;
        let installed: HashSet<(String, String)> = installed
            .iter()
            .map(|p| (p.server_id.to_string(), p.package_id.to_string()))
            .collect();
        let unused = self
            .db
            .call(move |conn| {
                let tx = conn.transaction()?;
                let refs: Vec<(String, String)> = {
                    let mut stmt =
                        tx.prepare("SELECT DISTINCT server_id, package_id FROM compat_runtime_refs")?;
                    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<Result<_, _>>()?
                };
                for (server, package) in refs.iter().filter(|r| !installed.contains(*r)) {
                    tx.execute(
                        "DELETE FROM compat_runtime_refs WHERE server_id = ?1 AND package_id = ?2",
                        params![server, package],
                    )?;
                }
                let unused: Vec<(String, String)> = {
                    let mut stmt = tx.prepare(
                        "SELECT r.runtime_id, r.version FROM compat_runtimes r
                          WHERE NOT EXISTS (SELECT 1 FROM compat_runtime_refs f
                                             WHERE f.runtime_id = r.runtime_id AND f.version = r.version)",
                    )?;
                    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<Result<_, _>>()?
                };
                tx.commit()?;
                Ok(unused)
            })
            .await?;
        for (id, version) in &unused {
            self.delete_version(id, version).await?;
        }
        Ok(unused)
    }

    /// Removes one version (`runtime_remove`); refused while a package uses it.
    pub async fn remove(&self, id: RuntimeId, version: &str) -> Result<(), RuntimeError> {
        let _busy = self.busy.lock().await;
        let used_by = used_by(&self.db, id.as_str(), version).await?;
        if used_by > 0 {
            return Err(RuntimeError::InUse { used_by });
        }
        self.delete_version(id.as_str(), version).await
    }

    async fn delete_version(&self, id: &str, version: &str) -> Result<(), RuntimeError> {
        // Ids and versions come from verified catalogs (no separators), but the
        // database is not trusted with paths: anything odd is never joined.
        let safe =
            |s: &str| !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\', '\0']);
        if safe(id) && safe(version) {
            let dir = self.root.join(id).join(version);
            if tokio::fs::try_exists(&dir).await? {
                remove_tree(dir).await?;
            }
        }
        let (id, version) = (id.to_owned(), version.to_owned());
        self.db
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM compat_runtimes WHERE runtime_id = ?1 AND version = ?2",
                    params![id, version],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Every installed runtime version with its use count.
    pub async fn list(&self) -> Result<Vec<InstalledRuntime>, RuntimeError> {
        Ok(self
            .db
            .call(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT r.runtime_id, r.version, r.size,
                            (SELECT COUNT(*) FROM compat_runtime_refs f
                              WHERE f.runtime_id = r.runtime_id AND f.version = r.version)
                       FROM compat_runtimes r ORDER BY r.runtime_id, r.version",
                )?;
                let rows = stmt
                    .query_map([], |r| {
                        Ok(InstalledRuntime {
                            runtime_id: r.get(0)?,
                            version: r.get(1)?,
                            size: u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
                            used_by: r.get(3)?,
                        })
                    })?
                    .collect::<Result<_, _>>()?;
                Ok(rows)
            })
            .await?)
    }
}

/// Wires the store to INS's after-uninstall hook.
pub fn on_uninstall(store: &Arc<RuntimeStore>) -> crate::installs::list::AfterUninstall {
    let store = Arc::clone(store);
    Arc::new(move |package| {
        let store = Arc::clone(&store);
        tauri::async_runtime::spawn(async move {
            if let Err(error) = store.package_removed(package).await {
                tracing::warn!(%error, "cannot drop the runtime references of an uninstalled package");
            }
        });
    })
}

/// Startup garbage collection, counting references from `db::installs`.
pub fn spawn_collect_garbage(store: Arc<RuntimeStore>, db: Db) {
    tauri::async_runtime::spawn(async move {
        let installed = match crate::db::installs::roots(&db).await {
            Ok(roots) => roots.into_iter().map(|(package, _)| package).collect(),
            Err(error) => {
                tracing::warn!(%error, "cannot list installs; runtime garbage collection skipped");
                return;
            }
        };
        match store.collect_garbage(&installed).await {
            Ok(removed) if !removed.is_empty() => {
                tracing::info!(
                    count = removed.len(),
                    "removed unused compatibility runtimes"
                );
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "runtime garbage collection failed"),
        }
    });
}

async fn remove_tree(dir: PathBuf) -> std::io::Result<()> {
    // `remove_dir_all` never follows links out of the tree.
    tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir))
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(buf.get(..n).unwrap_or_default());
    }
    Ok(hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

async fn is_recorded(db: &Db, rt: &Runtime) -> Result<bool, DbError> {
    let (id, version, sha) = (rt.id.as_str(), rt.version.clone(), rt.sha256.clone());
    db.call(move |conn| {
        Ok(conn
            .query_row(
                "SELECT 1 FROM compat_runtimes WHERE runtime_id = ?1 AND version = ?2 AND sha256 = ?3",
                params![id, version, sha],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    })
    .await
}

pub(super) async fn record(db: &Db, rt: &Runtime) -> Result<(), DbError> {
    let arch = match rt.arch {
        vgames_core::runtimes::Arch::X86_64 => "x86_64",
        vgames_core::runtimes::Arch::Aarch64 => "aarch64",
    };
    let (id, version, sha) = (rt.id.as_str(), rt.version.clone(), rt.sha256.clone());
    let size = i64::try_from(rt.size).unwrap_or(i64::MAX);
    db.call(move |conn| {
        conn.execute(
            "INSERT INTO compat_runtimes (runtime_id, version, arch, sha256, size, installed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (runtime_id, version) DO UPDATE SET
                 arch = excluded.arch, sha256 = excluded.sha256, size = excluded.size,
                 installed_at = excluded.installed_at",
            params![id, version, arch, sha, size, now_unix()],
        )?;
        Ok(())
    })
    .await
}

async fn add_ref(db: &Db, package: PackageRef, rt: &Runtime) -> Result<(), DbError> {
    let (server, pkg) = (
        package.server_id.to_string(),
        package.package_id.to_string(),
    );
    let (id, version) = (rt.id.as_str(), rt.version.clone());
    db.call(move |conn| {
        conn.execute(
            "INSERT INTO compat_runtime_refs (server_id, package_id, runtime_id, version)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (server_id, package_id, runtime_id) DO UPDATE SET version = excluded.version",
            params![server, pkg, id, version],
        )?;
        Ok(())
    })
    .await
}

async fn used_by(db: &Db, id: &str, version: &str) -> Result<u32, DbError> {
    let (id, version) = (id.to_owned(), version.to_owned());
    db.call(move |conn| {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM compat_runtime_refs WHERE runtime_id = ?1 AND version = ?2",
            params![id, version],
            |r| r.get(0),
        )?)
    })
    .await
}

#[cfg(test)]
mod tests;
