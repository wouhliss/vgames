//! Uninstall, move and open-folder (INS-04). Uninstall and move never follow
//! links: they go through `vgames_transfer::install::{preview_uninstall,
//! remove_install}` and `move_install::move_install`, which confine every
//! path to the install root.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use specta::Type;
use uuid::Uuid;
use vgames_core::manifest::{Manifest, Platform as Host};
use vgames_core::trust::TrustState;
use vgames_core::verify::{ExpectedRelease, VerifyMode};
use vgames_transfer::install::{self, InstallError};
use vgames_transfer::move_install::move_install;

use super::InstallActionError;
use crate::db::{self, Db};
use crate::events::PackageRef;
use crate::libraries::{self, LibraryPresence, LibraryRoot};

/// Leftovers shown in the uninstall dialog at most.
const SHOWN_LEFTOVERS: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct Leftover {
    pub path: String,
    pub size_bytes: u64,
}

/// What uninstalling removes, and what the player decides about (02 §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct UninstallPlan {
    pub size_bytes: u64,
    /// Files the manifest does not list (mods, configs, local saves): at most
    /// 50, relative paths.
    pub leftovers: Vec<Leftover>,
    pub leftover_count: u32,
    pub leftover_bytes: u64,
    /// A Proton/Wine prefix exists; it may hold local saves (09 §2).
    pub has_prefix: bool,
}

fn blocking_error(error: tokio::task::JoinError) -> InstallActionError {
    InstallActionError::io("finish the file operation", &error)
}

fn file_error(context: &str, error: InstallError) -> InstallActionError {
    match error {
        InstallError::NotEnoughSpace {
            required,
            available,
        } => InstallActionError::InsufficientSpace {
            required_bytes: required,
            available_bytes: available,
        },
        other => InstallActionError::io(context, &other),
    }
}

/// An install row that may be changed now: present, not running, not queued.
pub struct Target {
    pub row: db::installs::InstallRow,
    pub library: LibraryRoot,
}

pub async fn target(
    db: &Db,
    package: PackageRef,
    running: bool,
) -> Result<Target, InstallActionError> {
    let row = db::installs::row(db, package)
        .await
        .map_err(|e| InstallActionError::io("read the install", &e))?
        .ok_or(InstallActionError::NotFound)?;
    if running {
        return Err(InstallActionError::Running);
    }
    let queued = db::download_jobs::list(db)
        .await
        .map_err(|e| InstallActionError::io("read the download queue", &e))?
        .into_iter()
        .find(|job| job.package == package);
    if let Some(job) = queued {
        return Err(InstallActionError::Busy {
            state: match job.kind {
                db::download_jobs::JobKind::Install => super::InstallState::Installing,
                db::download_jobs::JobKind::Update => super::InstallState::Updating,
                db::download_jobs::JobKind::Repair => super::InstallState::Repairing,
            },
        });
    }
    if !matches!(row.state.as_str(), "installed" | "installing" | "broken") {
        return Err(InstallActionError::Busy {
            state: super::list::state_of(&row.state, None),
        });
    }
    let library_id = library_of(db, package).await?;
    let library = LibraryRoot {
        id: library_id,
        path: row.library_path.clone(),
    };
    let check = library.clone();
    let presence = tokio::task::spawn_blocking(move || libraries::inspect_root(&check))
        .await
        .map_err(blocking_error)?;
    if !matches!(presence, Ok(LibraryPresence::Online { .. })) {
        return Err(InstallActionError::LibraryOffline {
            library_path: library.path.to_string_lossy().into_owned(),
        });
    }
    Ok(Target { row, library })
}

async fn library_of(db: &Db, package: PackageRef) -> Result<Uuid, InstallActionError> {
    let id = db
        .call(move |conn| {
            Ok(conn.query_row(
                "SELECT library_id FROM installs WHERE server_id = ?1 AND package_id = ?2",
                [
                    package.server_id.to_string(),
                    package.package_id.to_string(),
                ],
                |r| r.get::<_, String>(0),
            )?)
        })
        .await
        .map_err(|e| InstallActionError::io("read the install", &e))?;
    Uuid::parse_str(&id).map_err(|_| InstallActionError::Io {
        detail: "The install record is damaged".into(),
    })
}

/// The stored manifest, for listing and removing its files. Removal only
/// deletes listed paths confined to the root, so it needs no signature
/// (an install whose key was revoked can still be uninstalled).
fn stored_manifest(root: &Path) -> Result<Option<Manifest>, InstallError> {
    let path = root.join(install::META_DIR).join(install::MANIFEST_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => vgames_core::manifest::parse_and_validate(&bytes)
            .map(Some)
            .map_err(|e| InstallError::Conflict(format!("the stored manifest is unreadable: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(InstallError::Io {
            op: "read",
            path,
            source,
        }),
    }
}

pub async fn uninstall_plan(
    target: &Target,
    prefix: PathBuf,
) -> Result<UninstallPlan, InstallActionError> {
    let root = target.row.root.clone();
    tokio::task::spawn_blocking(move || {
        let manifest = stored_manifest(&root).map_err(|e| file_error("read the install", e))?;
        let size_bytes = manifest
            .as_ref()
            .map_or(0, |m| m.files.iter().map(|f| f.size).sum());
        let preview = match &manifest {
            Some(manifest) => install::preview_uninstall(&root, manifest)
                .map_err(|e| file_error("list the leftovers", e))?,
            None => install::UninstallPreview::default(),
        };
        let mut leftover_bytes = 0u64;
        let mut leftovers = Vec::new();
        for path in &preview.paths {
            // Never follows a link: the link itself is the leftover.
            let size = std::fs::symlink_metadata(root.join(path))
                .map(|m| if m.is_file() { m.len() } else { 0 })
                .unwrap_or(0);
            leftover_bytes = leftover_bytes.saturating_add(size);
            if leftovers.len() < SHOWN_LEFTOVERS {
                leftovers.push(Leftover {
                    path: path.to_string_lossy().replace('\\', "/"),
                    size_bytes: size,
                });
            }
        }
        Ok(UninstallPlan {
            size_bytes,
            leftovers,
            leftover_count: u32::try_from(preview.paths.len()).unwrap_or(u32::MAX),
            leftover_bytes,
            has_prefix: std::fs::symlink_metadata(&prefix).is_ok_and(|m| m.is_dir()),
        })
    })
    .await
    .map_err(blocking_error)?
}

/// Removes the install's files (and, if asked, the leftovers and the
/// prefix) and forgets it. Links are never followed.
pub async fn uninstall(
    db: &Db,
    package: PackageRef,
    target: Target,
    remove_leftovers: bool,
    prefix: Option<PathBuf>,
) -> Result<(), InstallActionError> {
    db::installs::set_state(db, package, "uninstalling")
        .await
        .map_err(|e| InstallActionError::io("update the install", &e))?;
    let root = target.row.root.clone();
    let removed = tokio::task::spawn_blocking(move || -> Result<(), InstallError> {
        if std::fs::symlink_metadata(&root).is_ok() {
            if let Some(manifest) = stored_manifest(&root)? {
                install::remove_install(&root, &manifest)?;
            }
            if remove_leftovers {
                install::remove_tree_no_follow(&root)?;
            } else {
                // Kept leftovers keep their folder.
                let _ = std::fs::remove_dir(&root);
            }
        }
        if let Some(prefix) = prefix
            && std::fs::symlink_metadata(&prefix).is_ok()
        {
            install::remove_tree_no_follow(&prefix)?;
        }
        Ok(())
    })
    .await
    .map_err(blocking_error)?;
    if let Err(error) = removed {
        if let Err(reset) =
            db::installs::set_state(db, package, target_state(&target.row.state)).await
        {
            tracing::error!(%reset, "cannot restore the install state");
        }
        return Err(file_error("remove the files", error));
    }
    db::installs::delete(db, package)
        .await
        .map_err(|e| InstallActionError::io("forget the install", &e))
}

fn target_state(stored: &str) -> &'static str {
    match stored {
        "installing" => "installing",
        "broken" => "broken",
        _ => "installed",
    }
}

/// Moves an installed package into another library: a rename on one
/// filesystem, otherwise a copy where every file is verified before the
/// source is deleted. The manifest is verified under current trust first.
pub async fn move_to(
    db: &Db,
    package: PackageRef,
    target: Target,
    library_id: Uuid,
    trust: Arc<TrustState>,
) -> Result<PathBuf, InstallActionError> {
    if library_id == target.library.id {
        return Err(InstallActionError::SameLibrary);
    }
    if target.row.state != "installed" {
        return Err(InstallActionError::Busy {
            state: super::list::state_of(&target.row.state, None),
        });
    }
    let destination_library = db::libraries::list(db)
        .await
        .map_err(|e| InstallActionError::io("read the libraries", &e))?
        .into_iter()
        .find(|l| l.root.id == library_id)
        .ok_or(InstallActionError::NotFound)?;
    let check = destination_library.root.clone();
    let presence = tokio::task::spawn_blocking(move || libraries::inspect_root(&check))
        .await
        .map_err(blocking_error)?;
    if !matches!(presence, Ok(LibraryPresence::Online { .. })) {
        return Err(InstallActionError::LibraryOffline {
            library_path: destination_library.root.path.to_string_lossy().into_owned(),
        });
    }
    let expected = expected(package, &target.row)?;
    let dir_name = target
        .row
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| package.package_id.to_string());
    let taken = db
        .call({
            let library = library_id.to_string();
            move |conn| {
                let mut q = conn.prepare("SELECT dir_name FROM installs WHERE library_id = ?1")?;
                let names = q
                    .query_map([library], |r| r.get::<_, String>(0))?
                    .collect::<Result<std::collections::HashSet<_>, _>>()?;
                Ok(names)
            }
        })
        .await
        .map_err(|e| InstallActionError::io("read the installs", &e))?;
    db::installs::set_state(db, package, "moving")
        .await
        .map_err(|e| InstallActionError::io("update the install", &e))?;
    let source = target.row.root.clone();
    let library_path = destination_library.root.path.clone();
    let moved = tokio::task::spawn_blocking(move || -> Result<(String, PathBuf), InstallError> {
        let name = std::iter::once(dir_name.clone())
            .chain((2u32..10_000).map(|n| format!("{dir_name}-{n}")))
            .find(|n| {
                !taken.contains(n) && std::fs::symlink_metadata(library_path.join(n)).is_err()
            })
            .ok_or_else(|| InstallError::Conflict("no free folder name".into()))?;
        let release = install::load_local_release(&source, &trust, &expected, VerifyMode::Launch)?;
        let destination = library_path.join(&name);
        move_install(&source, &destination, release.manifest())?;
        Ok((name, destination))
    })
    .await
    .map_err(blocking_error)?;
    let (name, destination) = match moved {
        Ok(moved) => moved,
        Err(error) => {
            if let Err(reset) = db::installs::set_state(db, package, "installed").await {
                tracing::error!(%reset, "cannot restore the install state");
            }
            return Err(file_error("move the install", error));
        }
    };
    db::installs::set_location(db, package, library_id, name)
        .await
        .map_err(|e| InstallActionError::io("record the new location", &e))?;
    db::installs::set_state(db, package, "installed")
        .await
        .map_err(|e| InstallActionError::io("update the install", &e))?;
    Ok(destination)
}

fn expected(
    package: PackageRef,
    row: &db::installs::InstallRow,
) -> Result<ExpectedRelease, InstallActionError> {
    let damaged = || InstallActionError::Io {
        detail: "The install record is damaged".into(),
    };
    Ok(ExpectedRelease {
        server_id: package.server_id,
        package_id: package.package_id,
        version_id: Uuid::parse_str(&row.version_id).map_err(|_| damaged())?,
        platform: Host::ALL
            .into_iter()
            .find(|p| p.as_str() == row.platform)
            .ok_or_else(damaged)?,
        sequence: u64::try_from(row.sequence).map_err(|_| damaged())?,
    })
}

#[cfg(test)]
pub(crate) fn file_error_for_test(error: InstallError) -> InstallActionError {
    file_error("test", error)
}
