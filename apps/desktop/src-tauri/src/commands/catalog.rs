//! Catalog commands (INS-02): Browse, package details and the install dialog.

use tauri::State;
use uuid::Uuid;

use crate::catalog::{
    CatalogError, CatalogPage, CatalogQuery, GenreCount, InstallPlan, InstallPlanError,
    PackageDetails,
};
use crate::events::PackageRef;
use crate::installs::{self, InstallStartError};
use crate::state::AppState;

fn package_id(value: &str) -> Option<Uuid> {
    Uuid::parse_str(value).ok()
}

#[tauri::command]
#[specta::specta]
pub async fn catalog_list(
    state: State<'_, AppState>,
    query: CatalogQuery,
) -> Result<CatalogPage, CatalogError> {
    state.catalog.list(query).await
}

/// Genres in the catalog with package counts (for the filter).
#[tauri::command]
#[specta::specta]
pub async fn catalog_genres(state: State<'_, AppState>) -> Result<Vec<GenreCount>, CatalogError> {
    state.catalog.genres().await
}

#[tauri::command]
#[specta::specta]
pub async fn package_details(
    state: State<'_, AppState>,
    package_id: String,
) -> Result<PackageDetails, CatalogError> {
    let id = self::package_id(&package_id).ok_or(CatalogError::NotFound)?;
    state.catalog.details(id).await
}

/// Sizes for the install dialog. Nothing is downloaded yet.
#[tauri::command]
#[specta::specta]
pub async fn install_plan(
    state: State<'_, AppState>,
    package_id: String,
) -> Result<InstallPlan, InstallPlanError> {
    let id = self::package_id(&package_id).ok_or(InstallPlanError::NotFound)?;
    Ok(state.catalog.plan(id).await?.plan)
}

/// Queues the install into `library_id`; progress follows through
/// `install-progress`, the end through `install-finished`.
#[tauri::command]
#[specta::specta]
pub async fn install_start(
    state: State<'_, AppState>,
    package_id: String,
    library_id: String,
) -> Result<(), InstallStartError> {
    let package_id = self::package_id(&package_id).ok_or(InstallStartError::NotFound)?;
    let library_id = Uuid::parse_str(&library_id).map_err(|_| InstallStartError::Io {
        detail: "This library does not exist".into(),
    })?;
    let server_id = state
        .catalog
        .active_server()
        .await
        .map_err(|e| InstallStartError::from(crate::catalog::InstallPlanError::from(e)))?;
    installs::start(
        &state.catalog,
        &state.downloads,
        PackageRef {
            server_id,
            package_id,
        },
        library_id,
    )
    .await
}
