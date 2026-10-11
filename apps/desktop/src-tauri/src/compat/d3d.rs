//! The D3D check before a Windows build downloads on a Mac (09-compatibility §3,
//! GAME-06), as INS's [`PriorityHook`]: the launch executables come first, their PE
//! imports are scanned on a blocking thread, and a D3D12 title is refused with
//! `d3d12_unsupported_on_mac` before any other pack byte is requested (D3DMetal is
//! deferred, so no Mac runs D3D12 in v1). The result is recorded per (server,
//! package, version) for details, retries and the launch plan.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use rusqlite::{OptionalExtension, params};
use uuid::Uuid;
use vgames_core::manifest::{Manifest, Platform};

use super::pe::{self, D3dImports};
use crate::catalog::CompatBlocker;
use crate::db::{Db, DbError, now_unix};
use crate::downloads::hook::{CheckFuture, PriorityHook};
use crate::events::PackageRef;

/// Whether a build for `build` would run through Wine on `host`.
pub fn wine_on(host: Option<Platform>, build: Platform) -> bool {
    matches!(host, Some(Platform::MacosAarch64 | Platform::MacosX86_64))
        && matches!(build, Platform::WindowsX86_64)
}

/// The refusal for a scan result on a Mac.
pub fn blocker_for(imports: &D3dImports) -> Option<CompatBlocker> {
    imports
        .d3d12
        .then_some(CompatBlocker::D3d12UnsupportedOnMac)
}

pub struct D3dCheck {
    db: Db,
    host: Option<Platform>,
    /// The version each package's priority files belong to (set in `priority_files`).
    versions: Mutex<HashMap<PackageRef, Uuid>>,
}

impl D3dCheck {
    pub fn new(db: Db, host: Option<Platform>) -> Self {
        Self {
            db,
            host,
            versions: Mutex::new(HashMap::new()),
        }
    }

    async fn inspect(&self, package: PackageRef, paths: &[PathBuf]) -> Result<(), CompatBlocker> {
        let version = self
            .versions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&package);
        let owned: Vec<PathBuf> = paths.to_vec();
        let scanned = tokio::task::spawn_blocking(move || scan_all(&owned)).await;
        let imports = match scanned {
            Ok(Ok(imports)) => imports,
            Ok(Err(error)) => {
                // Not a PE we can read: Wine decides at launch; nothing to refuse here.
                tracing::info!(%error, "launch executable not scanned");
                return Ok(());
            }
            Err(error) => {
                tracing::warn!(%error, "the D3D scan stopped");
                return Ok(());
            }
        };
        if let Some(version) = version
            && let Err(error) = record(&self.db, package, version, imports).await
        {
            tracing::warn!(%error, "cannot record the D3D scan");
        }
        blocker_for(&imports).map_or(Ok(()), Err)
    }
}

/// Every launch target's executable, once.
pub fn launch_executables(manifest: &Manifest) -> Vec<String> {
    manifest
        .launch
        .as_ref()
        .map(|l| {
            l.targets
                .iter()
                .map(|t| t.executable.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        })
        .unwrap_or_default()
}

fn scan_all(paths: &[PathBuf]) -> Result<D3dImports, pe::PeError> {
    let mut all = D3dImports::default();
    for path in paths {
        let found = pe::scan_file(path)?;
        all.d3d9 |= found.d3d9;
        all.d3d10 |= found.d3d10;
        all.d3d11 |= found.d3d11;
        all.d3d12 |= found.d3d12;
    }
    Ok(all)
}

impl PriorityHook for D3dCheck {
    fn priority_files(&self, package: PackageRef, manifest: &Manifest) -> Vec<String> {
        if !wine_on(self.host, manifest.platform) {
            return Vec::new();
        }
        self.versions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(package, manifest.version_id);
        launch_executables(manifest)
    }

    fn check<'a>(&'a self, package: PackageRef, paths: &'a [PathBuf]) -> CheckFuture<'a> {
        Box::pin(self.inspect(package, paths))
    }
}

/// Scans `executable` now (a launch without a recorded result) and records it.
pub async fn scan_and_record(
    db: &Db,
    package: PackageRef,
    version: Uuid,
    executable: &Path,
) -> Result<D3dImports, pe::PeError> {
    let path = executable.to_owned();
    let imports = tokio::task::spawn_blocking(move || pe::scan_file(&path))
        .await
        .map_err(|e| pe::PeError::Io(e.to_string()))??;
    if let Err(error) = record(db, package, version, imports).await {
        tracing::warn!(%error, "cannot record the D3D scan");
    }
    Ok(imports)
}

pub async fn record(
    db: &Db,
    package: PackageRef,
    version: Uuid,
    imports: D3dImports,
) -> Result<(), DbError> {
    let ids = (
        package.server_id.to_string(),
        package.package_id.to_string(),
        version.to_string(),
    );
    db.call(move |conn| {
        conn.execute(
            "INSERT INTO compat_d3d_scans
                 (server_id, package_id, version_id, d3d9, d3d10, d3d11, d3d12, scanned_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (server_id, package_id, version_id) DO UPDATE SET
                 d3d9 = excluded.d3d9, d3d10 = excluded.d3d10, d3d11 = excluded.d3d11,
                 d3d12 = excluded.d3d12, scanned_at = excluded.scanned_at",
            params![
                ids.0,
                ids.1,
                ids.2,
                imports.d3d9,
                imports.d3d10,
                imports.d3d11,
                imports.d3d12,
                now_unix()
            ],
        )?;
        Ok(())
    })
    .await
}

/// The recorded scan of a release, if any.
pub async fn recorded(
    db: &Db,
    package: PackageRef,
    version: Uuid,
) -> Result<Option<D3dImports>, DbError> {
    let ids = (
        package.server_id.to_string(),
        package.package_id.to_string(),
        version.to_string(),
    );
    db.call(move |conn| {
        Ok(conn
            .query_row(
                "SELECT d3d9, d3d10, d3d11, d3d12 FROM compat_d3d_scans
                  WHERE server_id = ?1 AND package_id = ?2 AND version_id = ?3",
                params![ids.0, ids.1, ids.2],
                |r| {
                    Ok(D3dImports {
                        d3d9: r.get(0)?,
                        d3d10: r.get(1)?,
                        d3d11: r.get(2)?,
                        d3d12: r.get(3)?,
                    })
                },
            )
            .optional()?)
    })
    .await
}

#[cfg(test)]
mod tests;
