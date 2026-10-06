//! Collections and favorites (INS-04), over `db::collections`. Collections
//! are local and span servers; favorites belong to one server's package.
//! Package arguments are `pkg`: `package` is reserved in strict TypeScript.

use serde::Serialize;
use specta::Type;
use tauri::State;
use uuid::Uuid;

use crate::db::collections::{self as store, CollectionStoreError};
use crate::error::AppError;
use crate::events::{AppEvent, CollectionsChanged, InstallsChanged, PackageRef};
use crate::state::AppState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct Collection {
    pub id: String,
    /// 1–100 characters.
    pub name: String,
    pub position: u32,
}

impl From<store::Collection> for Collection {
    fn from(c: store::Collection) -> Self {
        Self {
            id: c.id.to_string(),
            name: c.name,
            position: u32::try_from(c.position).unwrap_or(u32::MAX),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CollectionError {
    #[error("the name must be 1 to 100 characters")]
    InvalidName,
    #[error("a collection with this name exists")]
    NameTaken,
    #[error("the collection does not exist")]
    NotFound,
    /// The launcher could not save the change (details in the log).
    #[error("{detail}")]
    Io { detail: String },
}

fn map(error: CollectionStoreError) -> CollectionError {
    match error {
        CollectionStoreError::InvalidName => CollectionError::InvalidName,
        CollectionStoreError::NameTaken => CollectionError::NameTaken,
        CollectionStoreError::NotFound => CollectionError::NotFound,
        other => {
            tracing::error!(error = %crate::error::DisplayChain(&other), "collection change failed");
            CollectionError::Io {
                detail: "Cannot save the collection. See the log for details.".into(),
            }
        }
    }
}

fn collection_id(value: &str) -> Result<Uuid, CollectionError> {
    Uuid::parse_str(value).map_err(|_| CollectionError::NotFound)
}

fn changed(state: &AppState) {
    state
        .bus
        .publish(AppEvent::CollectionsChanged(CollectionsChanged {}));
}

#[tauri::command]
#[specta::specta]
pub async fn collections_list(state: State<'_, AppState>) -> Result<Vec<Collection>, AppError> {
    Ok(store::list(&state.db)
        .await
        .map_err(|e| AppError::internal("read the collections", &e))?
        .into_iter()
        .map(Into::into)
        .collect())
}

#[tauri::command]
#[specta::specta]
pub async fn collection_create(
    state: State<'_, AppState>,
    name: String,
) -> Result<Collection, CollectionError> {
    let collection = store::create(&state.db, &name).await.map_err(map)?;
    changed(&state);
    Ok(collection.into())
}

#[tauri::command]
#[specta::specta]
pub async fn collection_rename(
    state: State<'_, AppState>,
    collection_id: String,
    name: String,
) -> Result<Collection, CollectionError> {
    let id = self::collection_id(&collection_id)?;
    let collection = store::rename(&state.db, id, &name).await.map_err(map)?;
    changed(&state);
    Ok(collection.into())
}

#[tauri::command]
#[specta::specta]
pub async fn collection_delete(
    state: State<'_, AppState>,
    collection_id: String,
) -> Result<(), CollectionError> {
    let id = self::collection_id(&collection_id)?;
    store::delete(&state.db, id).await.map_err(map)?;
    changed(&state);
    Ok(())
}

/// The full new order; ids not listed keep their relative order after the listed ones.
#[tauri::command]
#[specta::specta]
pub async fn collections_reorder(
    state: State<'_, AppState>,
    collection_ids: Vec<String>,
) -> Result<(), CollectionError> {
    if collection_ids.len() > 10_000 {
        return Err(CollectionError::NotFound);
    }
    let ids = collection_ids
        .iter()
        .map(|id| self::collection_id(id))
        .collect::<Result<Vec<_>, _>>()?;
    store::reorder(&state.db, ids).await.map_err(map)?;
    changed(&state);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn collection_add_package(
    state: State<'_, AppState>,
    collection_id: String,
    pkg: PackageRef,
) -> Result<(), CollectionError> {
    let id = self::collection_id(&collection_id)?;
    store::add_package(&state.db, id, pkg).await.map_err(map)?;
    changed(&state);
    state
        .bus
        .publish(AppEvent::InstallsChanged(InstallsChanged {}));
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn collection_remove_package(
    state: State<'_, AppState>,
    collection_id: String,
    pkg: PackageRef,
) -> Result<(), CollectionError> {
    let id = self::collection_id(&collection_id)?;
    store::remove_package(&state.db, id, pkg)
        .await
        .map_err(map)?;
    changed(&state);
    state
        .bus
        .publish(AppEvent::InstallsChanged(InstallsChanged {}));
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn favorite_set(
    state: State<'_, AppState>,
    pkg: PackageRef,
    favorite: bool,
) -> Result<(), AppError> {
    store::set_favorite(&state.db, pkg, favorite)
        .await
        .map_err(|e| match e {
            CollectionStoreError::NotFound => AppError::NotFound,
            other => AppError::internal("save the favorite", &other),
        })?;
    state
        .bus
        .publish(AppEvent::InstallsChanged(InstallsChanged {}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_errors_map_to_the_ui_kinds() {
        assert_eq!(
            map(CollectionStoreError::InvalidName),
            CollectionError::InvalidName
        );
        assert_eq!(
            map(CollectionStoreError::NameTaken),
            CollectionError::NameTaken
        );
        assert_eq!(
            map(CollectionStoreError::NotFound),
            CollectionError::NotFound
        );
        // A local failure is never shown as a missing collection.
        assert!(matches!(
            map(CollectionStoreError::Database(crate::db::DbError::Closed)),
            CollectionError::Io { .. }
        ));
        assert_eq!(collection_id("not-a-uuid"), Err(CollectionError::NotFound));
    }
}
