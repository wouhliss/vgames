//! Launch and stop commands (A2-T09).

use tauri::State;

use crate::error::AppError;
use crate::events::PackageRef;
use crate::launch::orchestrate::LaunchError;
use crate::launch::{SessionError, TargetChoice};
use crate::state::AppState;

/// Pre-launch checks, then spawn. `target_id` null = the default target.
// `pkg`, not `package`: that is a reserved word in strict-mode TypeScript.
#[tauri::command]
#[specta::specta]
pub async fn game_launch(
    state: State<'_, AppState>,
    pkg: PackageRef,
    target_id: Option<String>,
) -> Result<(), LaunchError> {
    let choice = target_id.map_or(TargetChoice::Default, TargetChoice::Target);
    state.launcher.launch(pkg, choice).await.map(|_pid| ())
}

/// Ends the game's whole process tree. The UI confirms first.
#[tauri::command]
#[specta::specta]
pub async fn game_stop(state: State<'_, AppState>, pkg: PackageRef) -> Result<(), AppError> {
    match state.games.stop(pkg, false) {
        Ok(()) => Ok(()),
        Err(SessionError::NotRunning) => Err(AppError::NotFound),
        Err(error) => Err(AppError::internal("stop the game", &error)),
    }
}
