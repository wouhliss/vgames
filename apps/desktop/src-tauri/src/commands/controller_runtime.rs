//! Cached controller list and scoped tester subscriptions. SDL stays on its owner.
use tauri::State;
use uuid::Uuid;

use crate::controllers::handle::ControllerList;
use crate::controllers::routing::Tester;
use crate::controllers::service::ServiceError;
use crate::error::AppError;
use crate::state::AppState;

#[tauri::command]
#[specta::specta]
pub async fn controllers_list(state: State<'_, AppState>) -> Result<ControllerList, AppError> {
    Ok(state.controllers.list())
}
#[tauri::command]
#[specta::specta]
pub async fn controllers_tester_start(
    state: State<'_, AppState>,
    instance_id: u32,
) -> Result<Tester, AppError> {
    state
        .controllers
        .tester_start(instance_id)
        .map_err(tester_error)
}
#[tauri::command]
#[specta::specta]
pub async fn controllers_tester_stop(
    state: State<'_, AppState>,
    subscription_id: Uuid,
) -> Result<(), AppError> {
    state
        .controllers
        .tester_stop(subscription_id)
        .map_err(tester_error)
}
fn tester_error(error: ServiceError) -> AppError {
    match error {
        ServiceError::Tester(error) => AppError::invalid("instance_id", error.to_string()),
        other => AppError::internal("controller tester", &other),
    }
}
