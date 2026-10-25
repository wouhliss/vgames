use crate::controllers::drivers::{self, ControllerDriverStatus};
use crate::error::AppError;

#[tauri::command]
#[specta::specta]
pub async fn controllers_driver_status() -> Result<ControllerDriverStatus, AppError> {
    tokio::task::spawn_blocking(drivers::probe)
        .await
        .map_err(|error| AppError::internal("probe controller drivers", &error))
}
