//! Queueing updates, repairs and resumed installs (INS-04). Each goes
//! through the queue worker, so progress and the end arrive as
//! `install-progress` and `install-finished` like an install.

use serde::Serialize;
use specta::Type;
use uuid::Uuid;

use super::InstallState;
use crate::db::download_jobs::{self as jobs, JobKind, JobOptions, JobStoreError};
use crate::db::{self, Db};
use crate::events::PackageRef;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstallActionError {
    #[error("the package is not installed")]
    NotFound,
    #[error("the install is busy")]
    Busy { state: InstallState },
    #[error("the game is running")]
    Running,
    #[error("the library is offline")]
    LibraryOffline { library_path: String },
    #[error("the server cannot be reached")]
    Offline,
    #[error("not enough disk space")]
    InsufficientSpace {
        required_bytes: u64,
        available_bytes: u64,
    },
    #[error("the package is already in this library")]
    SameLibrary,
    #[error("{detail}")]
    Io { detail: String },
}

impl InstallActionError {
    pub fn io(context: &str, error: &dyn std::error::Error) -> Self {
        tracing::error!(error = %crate::error::DisplayChain(error), "cannot {context}");
        Self::Io {
            detail: format!("Cannot {context}. See the log for details."),
        }
    }
}

fn busy(stored: &str) -> InstallState {
    match stored {
        "installing" | "broken" => InstallState::Incomplete,
        "updating" => InstallState::Updating,
        "repairing" => InstallState::Repairing,
        "moving" => InstallState::Moving,
        "uninstalling" => InstallState::Uninstalling,
        _ => InstallState::Installed,
    }
}

/// Queues an update, a repair (verify) or a resumed install.
pub async fn queue(
    db: &Db,
    package: PackageRef,
    kind: JobKind,
    running: bool,
) -> Result<(), InstallActionError> {
    let row = db::installs::row(db, package)
        .await
        .map_err(|e| InstallActionError::io("read the install", &e))?
        .ok_or(InstallActionError::NotFound)?;
    let wanted = match kind {
        JobKind::Install => "installing",
        JobKind::Update | JobKind::Repair => "installed",
    };
    if row.state != wanted {
        return Err(InstallActionError::Busy {
            state: busy(&row.state),
        });
    }
    if running && kind != JobKind::Install {
        return Err(InstallActionError::Running);
    }
    let library_id = db
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
    let (Ok(library_id), Ok(version_id)) = (
        Uuid::parse_str(&library_id),
        Uuid::parse_str(&row.version_id),
    ) else {
        return Err(InstallActionError::Io {
            detail: "The install record is damaged".into(),
        });
    };
    match jobs::enqueue(
        db,
        package,
        version_id,
        library_id,
        kind,
        JobOptions::default(),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(JobStoreError::AlreadyQueued) => Err(InstallActionError::Busy {
            state: match kind {
                JobKind::Install => InstallState::Installing,
                JobKind::Update => InstallState::Updating,
                JobKind::Repair => InstallState::Repairing,
            },
        }),
        Err(error) => Err(InstallActionError::io("queue the job", &error)),
    }
}
