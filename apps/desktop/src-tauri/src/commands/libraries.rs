//! Library commands. Paths and disk inspection stay in Rust; SQLite work runs
//! on the database thread and filesystem inspection runs on the blocking pool.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

use crate::db::libraries::{self as store, Library, LibraryStoreError};
use crate::error::{CommandError, CommandResult, ErrorCode};
use crate::libraries::{self, LibraryError, LibraryPresence};
use crate::state::AppState;

#[derive(Debug, Serialize, Type)]
pub struct LibraryInfo {
    pub id: String,
    pub path: String,
    pub label: Option<String>,
    pub is_default: bool,
    pub online: bool,
    pub free_bytes: Option<u64>,
    /// Reserved for a filesystem capacity measurement; absent when unavailable.
    pub total_bytes: Option<u64>,
    pub install_count: u64,
}

fn library_info(library: Library, presence: LibraryPresence, install_count: u64) -> LibraryInfo {
    let (online, free_bytes, total_bytes) = match presence {
        LibraryPresence::Online {
            free_bytes,
            total_bytes,
        } => (true, Some(free_bytes), Some(total_bytes)),
        LibraryPresence::Offline | LibraryPresence::MarkerChanged => (false, None, None),
    };
    LibraryInfo {
        id: library.root.id.to_string(),
        path: library.root.path.to_string_lossy().into_owned(),
        label: Some(library.label),
        is_default: library.is_default,
        online,
        free_bytes,
        total_bytes,
        install_count,
    }
}

#[derive(Debug, Serialize, Type)]
pub struct ChosenLibraryFolder {
    pub path: String,
    pub free_bytes: u64,
    pub total_bytes: u64,
}

#[tauri::command]
#[specta::specta]
pub async fn library_pick_folder(
    app: AppHandle,
) -> Result<Option<ChosenLibraryFolder>, LibraryActionError> {
    let (reply, selected) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |path| {
        let _ = reply.send(path);
    });
    let path = selected
        .await
        .map_err(|error| LibraryActionError::io("Cannot open the folder picker", &error))?;
    let Some(path) = path else {
        return Ok(None);
    };
    let path = path
        .into_path()
        .map_err(|error| LibraryActionError::io("Cannot read the chosen folder", &error))?;
    tokio::task::spawn_blocking(move || {
        let path = std::fs::canonicalize(&path)
            .map_err(|error| LibraryActionError::io("Cannot open the chosen folder", &error))?;
        let free_bytes = vgames_transfer::sys::available_space(&path)
            .map_err(|error| LibraryActionError::io("Cannot inspect free space", &error))?;
        let total_bytes = vgames_transfer::sys::total_space(&path)
            .map_err(|error| LibraryActionError::io("Cannot inspect capacity", &error))?;
        Ok(Some(ChosenLibraryFolder {
            path: path.to_string_lossy().into_owned(),
            free_bytes,
            total_bytes,
        }))
    })
    .await
    .map_err(|error| LibraryActionError::io("Cannot inspect the chosen folder", &error))?
}

/// Errors expected by the launcher's library screens.
#[derive(Debug, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LibraryActionError {
    #[error("The folder is not writable")]
    NotWritable,
    #[error("System folders cannot be used as libraries")]
    SystemDirectory,
    #[error("The folder is inside another library")]
    NestedInLibrary { library_path: String },
    #[error("The folder contains another library")]
    ContainsLibrary { library_path: String },
    #[error("The folder is already a library")]
    AlreadyAdded,
    #[error("The library does not exist")]
    NotFound,
    #[error("{detail}")]
    Io { detail: String },
}

impl LibraryActionError {
    fn io(context: &str, error: &dyn std::error::Error) -> Self {
        tracing::error!(%error, "{context}");
        Self::Io {
            detail: context.to_owned(),
        }
    }
}

#[derive(Debug, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LibraryRemovalError {
    #[error("The folder is not writable")]
    NotWritable,
    #[error("System folders cannot be used as libraries")]
    SystemDirectory,
    #[error("The folder is inside another library")]
    NestedInLibrary { library_path: String },
    #[error("The folder contains another library")]
    ContainsLibrary { library_path: String },
    #[error("The folder is already a library")]
    AlreadyAdded,
    #[error("The library does not exist")]
    NotFound,
    #[error("{detail}")]
    Io { detail: String },
    #[error("Move or uninstall the {install_count} installed packages first")]
    NotEmpty { install_count: u64 },
}

impl From<LibraryActionError> for LibraryRemovalError {
    fn from(error: LibraryActionError) -> Self {
        match error {
            LibraryActionError::NotWritable => Self::NotWritable,
            LibraryActionError::SystemDirectory => Self::SystemDirectory,
            LibraryActionError::NestedInLibrary { library_path } => {
                Self::NestedInLibrary { library_path }
            }
            LibraryActionError::ContainsLibrary { library_path } => {
                Self::ContainsLibrary { library_path }
            }
            LibraryActionError::AlreadyAdded => Self::AlreadyAdded,
            LibraryActionError::NotFound => Self::NotFound,
            LibraryActionError::Io { detail } => Self::Io { detail },
        }
    }
}

fn map_error(error: LibraryStoreError) -> CommandError {
    match error {
        LibraryStoreError::InvalidLabel | LibraryStoreError::NonUnicodePath => {
            CommandError::invalid(error.to_string())
        }
        LibraryStoreError::NotFound => CommandError::new(ErrorCode::NotFound, error.to_string()),
        LibraryStoreError::InUse => CommandError::new(ErrorCode::Conflict, error.to_string()),
        LibraryStoreError::Root(root) => map_root_error(root),
        other => CommandError::internal("Cannot change libraries", &other),
    }
}

fn map_root_error(error: LibraryError) -> CommandError {
    match error {
        LibraryError::SystemFolder | LibraryError::NotDirectory => {
            CommandError::invalid(error.to_string())
        }
        LibraryError::Overlap { .. } | LibraryError::AlreadyRegistered => {
            CommandError::new(ErrorCode::Conflict, error.to_string())
        }
        LibraryError::InvalidMarker => CommandError::new(ErrorCode::Integrity, error.to_string()),
        other => CommandError::internal("Cannot inspect the library folder", &other),
    }
}

fn map_action_error(error: LibraryStoreError) -> LibraryActionError {
    match error {
        LibraryStoreError::NotFound => LibraryActionError::NotFound,
        LibraryStoreError::Root(LibraryError::SystemFolder) => LibraryActionError::SystemDirectory,
        LibraryStoreError::Root(LibraryError::NotDirectory) => LibraryActionError::NotFound,
        LibraryStoreError::Root(LibraryError::Overlap { other, nested }) => {
            let library_path = other.to_string_lossy().into_owned();
            if nested {
                LibraryActionError::NestedInLibrary { library_path }
            } else {
                LibraryActionError::ContainsLibrary { library_path }
            }
        }
        LibraryStoreError::Root(LibraryError::AlreadyRegistered) => {
            LibraryActionError::AlreadyAdded
        }
        LibraryStoreError::Root(LibraryError::Io { action, source, .. })
            if action == "test writing to"
                || source.kind() == std::io::ErrorKind::PermissionDenied =>
        {
            LibraryActionError::NotWritable
        }
        other => LibraryActionError::io("Cannot change libraries", &other),
    }
}

fn parse_action_id(id: &str) -> Result<Uuid, LibraryActionError> {
    Uuid::parse_str(id).map_err(|_| LibraryActionError::NotFound)
}

#[tauri::command]
#[specta::specta]
pub async fn libraries_list(state: State<'_, AppState>) -> CommandResult<Vec<LibraryInfo>> {
    // A library that reappeared resumes its paused downloads (INS-03).
    let downloads = std::sync::Arc::clone(&state.downloads);
    tauri::async_runtime::spawn(async move { downloads.recheck().await });
    let libraries = store::list(&state.db).await.map_err(map_error)?;
    let counts: HashMap<Uuid, u64> = store::install_counts(&state.db).await.map_err(map_error)?;
    tokio::task::spawn_blocking(move || {
        libraries
            .into_iter()
            .map(|library| {
                let count = counts.get(&library.root.id).copied().unwrap_or(0);
                let presence = match libraries::inspect_root(&library.root) {
                    Ok(presence) => presence,
                    Err(error) => {
                        tracing::warn!(library_id = %library.root.id, %error, "library is unavailable");
                        LibraryPresence::Offline
                    }
                };
                library_info(library, presence, count)
            })
            .collect()
    })
    .await
    .map_err(|error| CommandError::internal("Cannot inspect libraries", &error))
}

#[tauri::command]
#[specta::specta]
pub async fn library_add(
    path: String,
    make_default: bool,
    state: State<'_, AppState>,
) -> Result<LibraryInfo, LibraryActionError> {
    if path.is_empty() {
        return Err(LibraryActionError::NotFound);
    }
    let path = PathBuf::from(path);
    let label = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Library");
    let library = store::add_with_default(&state.db, &path, label, make_default)
        .await
        .map_err(map_action_error)?;
    let presence = tokio::task::spawn_blocking({
        let root = library.root.clone();
        move || libraries::inspect_root(&root)
    })
    .await
    .map_err(|error| LibraryActionError::io("Cannot inspect the new library", &error))?;
    let presence = match presence {
        Ok(presence) => presence,
        Err(error) => {
            tracing::warn!(library_id = %library.root.id, %error, "new library is unavailable");
            LibraryPresence::Offline
        }
    };
    Ok(library_info(library, presence, 0))
}

#[tauri::command]
#[specta::specta]
pub async fn library_set_default(
    library_id: String,
    state: State<'_, AppState>,
) -> Result<(), LibraryActionError> {
    let id = parse_action_id(&library_id)?;
    store::set_default(&state.db, id)
        .await
        .map_err(map_action_error)
}

#[tauri::command]
#[specta::specta]
pub async fn library_remove(
    library_id: String,
    state: State<'_, AppState>,
) -> Result<(), LibraryRemovalError> {
    let id = parse_action_id(&library_id).map_err(LibraryRemovalError::from)?;
    match store::remove(&state.db, id).await {
        Ok(_) => {}
        Err(LibraryStoreError::InUse) => {
            let counts = store::install_counts(&state.db)
                .await
                .map_err(map_action_error)
                .map_err(LibraryRemovalError::from)?;
            let count = counts.get(&id).copied().unwrap_or(0);
            if count > 0 {
                return Err(LibraryRemovalError::NotEmpty {
                    install_count: count,
                });
            }
            return Err(LibraryRemovalError::Io {
                detail: "Finish or cancel downloads in this library before removing it.".into(),
            });
        }
        Err(error) => return Err(LibraryRemovalError::from(map_action_error(error))),
    }
    Ok(())
}
