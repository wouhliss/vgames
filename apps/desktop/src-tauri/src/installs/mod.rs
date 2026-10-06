//! Installed packages (INS). INS-03: [`start`], the one way an install
//! begins, whatever asked for it (the install dialog, an invite, a deep link):
//! plan → checks → register the install and queue its job. Progress and the
//! end are the queue worker's `install-progress` and `install-finished`.

pub mod actions;
pub mod list;

pub use actions::InstallActionError;
pub use list::*;

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;
use specta::Type;
use uuid::Uuid;

use crate::catalog::{Catalog, CompatBlocker, Connections, InstallPlanError};
use crate::db::download_jobs::{self as jobs, JobStoreError, NewInstall};
use crate::db::{self, Db};
use crate::downloads::Downloads;
use crate::downloads::backend::Backend;
use crate::events::PackageRef;
use crate::libraries::{self, LibraryPresence};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstallStartError {
    #[error("the package does not exist")]
    NotFound,
    /// No build for this machine and no compatibility path.
    #[error("no build runs on this computer")]
    NoRelease,
    #[error("the package is already installed")]
    AlreadyInstalled,
    #[error("the server cannot be reached")]
    Offline,
    #[error("the package cannot run on this computer")]
    Blocked { blocker: CompatBlocker },
    #[error("{message} ({code})")]
    Server { code: String, message: String },
    #[error("not enough disk space")]
    InsufficientSpace {
        required_bytes: u64,
        available_bytes: u64,
    },
    #[error("the library is offline")]
    LibraryOffline { library_path: String },
    /// The server's trust bundle expired: installed games still launch, new
    /// installs wait (01-security §3.2).
    #[error("the server's trust bundle expired")]
    TrustExpired,
    #[error("{detail}")]
    Io { detail: String },
}

impl From<InstallPlanError> for InstallStartError {
    fn from(error: InstallPlanError) -> Self {
        match error {
            InstallPlanError::NotFound => Self::NotFound,
            InstallPlanError::NoRelease => Self::NoRelease,
            InstallPlanError::AlreadyInstalled => Self::AlreadyInstalled,
            InstallPlanError::Offline => Self::Offline,
            InstallPlanError::Blocked { blocker } => Self::Blocked { blocker },
            InstallPlanError::Server { code, message } => Self::Server { code, message },
        }
    }
}

fn io(context: &str, error: &dyn std::error::Error) -> InstallStartError {
    tracing::error!(error = %crate::error::DisplayChain(error), "cannot {context}");
    InstallStartError::Io {
        detail: format!("Cannot {context}"),
    }
}

/// Starts installing `package` into the library `library_id`: the plan
/// (release for this host, hard compat blockers), trust, the library and its
/// free space, then the install is registered (`installing`) and queued.
pub async fn start<C: Connections, B: Backend>(
    catalog: &Catalog<C>,
    downloads: &Downloads<B>,
    package: PackageRef,
    library_id: Uuid,
) -> Result<(), InstallStartError> {
    let planned = catalog.plan_for(package).await?;
    let trust = downloads
        .backend()
        .trust(package.server_id, false)
        .await
        .map_err(|e| io("read the trust state", &e))?;
    if trust
        .is_some_and(|t| t.is_expired(vgames_core::Timestamp::new(time::OffsetDateTime::now_utc())))
    {
        return Err(InstallStartError::TrustExpired);
    }
    let db = downloads.db();
    let library = db::libraries::list(db)
        .await
        .map_err(|e| io("read the libraries", &e))?
        .into_iter()
        .find(|l| l.root.id == library_id)
        .ok_or(InstallStartError::Io {
            detail: "This library does not exist".into(),
        })?;
    let library_path = library.root.path.clone();
    let root = library.root.clone();
    let presence = tokio::task::spawn_blocking(move || libraries::inspect_root(&root))
        .await
        .map_err(|e| io("inspect the library", &e))?;
    let free_bytes = match presence {
        Ok(LibraryPresence::Online { .. }) => downloads.free_space(&library_path),
        Ok(LibraryPresence::Offline | LibraryPresence::MarkerChanged) | Err(_) => {
            return Err(InstallStartError::LibraryOffline {
                library_path: library_path.to_string_lossy().into_owned(),
            });
        }
    };
    if free_bytes < planned.plan.required_bytes {
        return Err(InstallStartError::InsufficientSpace {
            required_bytes: planned.plan.required_bytes,
            available_bytes: free_bytes,
        });
    }
    let detail = &planned.detail.summary;
    let dir_name = free_dir_name(
        db,
        library_id,
        &library_path,
        &detail.slug,
        package.package_id,
    )
    .await?;
    let new = NewInstall {
        package,
        library_id,
        dir_name,
        version_id: planned.release.version_id,
        sequence: planned.release.sequence,
        platform: planned.release.platform.as_str().to_owned(),
        size_bytes: planned.release.total_size,
        title: detail.title.clone(),
        slug: detail.slug.clone(),
        version_label: planned.release.version_label.clone(),
        cover_asset_id: detail.cover.as_ref().map(|c| c.id),
    };
    match jobs::begin_install(db, new).await {
        Ok(_) => {}
        Err(JobStoreError::AlreadyInstalled | JobStoreError::AlreadyQueued) => {
            return Err(InstallStartError::AlreadyInstalled);
        }
        Err(error) => return Err(io("queue the install", &error)),
    }
    tracing::info!(%package.package_id, %library_id, "install queued");
    downloads.queued();
    Ok(())
}

/// A server slug is untrusted: only a plain name becomes a folder.
fn safe_slug(slug: &str, package_id: Uuid) -> String {
    let plain = !slug.is_empty()
        && slug.len() <= 64
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !slug.starts_with('-');
    if plain {
        slug.to_owned()
    } else {
        package_id.to_string()
    }
}

/// `<slug>`, else `<slug>-2`, `<slug>-3`, …: free on disk and not registered
/// for another install of the library.
async fn free_dir_name(
    db: &Db,
    library_id: Uuid,
    library: &Path,
    slug: &str,
    package_id: Uuid,
) -> Result<String, InstallStartError> {
    let slug = safe_slug(slug, package_id);
    let library_key = library_id.to_string();
    let taken: HashSet<String> = db
        .call(move |conn| {
            let mut query = conn.prepare("SELECT dir_name FROM installs WHERE library_id = ?1")?;
            let names = query
                .query_map([library_key], |row| row.get::<_, String>(0))?
                .collect::<Result<HashSet<_>, _>>()?;
            Ok(names)
        })
        .await
        .map_err(|e| io("read the installs", &e))?;
    let library = library.to_owned();
    tokio::task::spawn_blocking(move || {
        std::iter::once(slug.clone())
            .chain((2u32..10_000).map(|n| format!("{slug}-{n}")))
            .find(|name| {
                !taken.contains(name) && std::fs::symlink_metadata(library.join(name)).is_err()
            })
            .unwrap_or_else(|| format!("{slug}-{}", Uuid::now_v7()))
    })
    .await
    .map_err(|e| io("choose a folder", &e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_slugs_become_folders() {
        let id = Uuid::now_v7();
        assert_eq!(safe_slug("gilded-garden-2", id), "gilded-garden-2");
        for unsafe_slug in [
            "",
            "../evil",
            "a/b",
            "C:",
            "-x",
            ".",
            "a b",
            "É",
            &"x".repeat(65),
        ] {
            assert_eq!(
                safe_slug(unsafe_slug, id),
                id.to_string(),
                "{unsafe_slug:?}"
            );
        }
    }
}
