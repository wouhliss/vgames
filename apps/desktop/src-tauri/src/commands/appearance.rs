//! Appearance preferences use the existing serialized database worker.
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager, State};

use crate::db::{Db, DbError, settings};
use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
    HighContrast,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AppearanceSettings {
    pub theme: Theme,
    pub reduce_motion: bool,
}
struct Appearance;
impl settings::Setting for Appearance {
    const KEY: &'static str = "app.appearance";
    type Value = AppearanceSettings;
    fn default_value() -> Self::Value {
        AppearanceSettings::default()
    }
}
async fn get(db: &Db) -> Result<AppearanceSettings, DbError> {
    db.call(|conn| settings::get::<Appearance>(conn)).await
}
async fn set(db: &Db, value: AppearanceSettings) -> Result<AppearanceSettings, DbError> {
    db.call(move |conn| {
        settings::set::<Appearance>(conn, &value)?;
        Ok(value)
    })
    .await
}
#[tauri::command]
#[specta::specta]
pub async fn appearance_get(app: AppHandle) -> AppearanceSettings {
    let db = app.state::<AppState>().db.clone();
    get(&db).await.unwrap_or_else(|_| {
        tracing::warn!("appearance preferences are unavailable");
        AppearanceSettings::default()
    })
}
#[tauri::command]
#[specta::specta]
pub async fn appearance_set(
    state: State<'_, AppState>,
    settings: AppearanceSettings,
) -> Result<AppearanceSettings, AppError> {
    set(&state.db, settings)
        .await
        .map_err(|_| AppError::Internal {
            detail: "Cannot save appearance preferences.".into(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn every_theme_and_motion_preference_persist_across_database_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.db");
        for theme in [
            Theme::System,
            Theme::Dark,
            Theme::Light,
            Theme::HighContrast,
        ] {
            let db = Db::open(&path).unwrap();
            let value = AppearanceSettings {
                theme,
                reduce_motion: true,
            };
            assert_eq!(set(&db, value).await.unwrap(), value);
            drop(db);
            let reopened = Db::open(&path).unwrap();
            assert_eq!(get(&reopened).await.unwrap(), value);
        }
    }
    #[tokio::test]
    async fn missing_or_corrupt_preferences_use_the_system_default() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(get(&db).await.unwrap(), AppearanceSettings::default());
        db.call(|conn| { conn.execute("INSERT INTO settings (key, value, updated_at) VALUES ('app.appearance', '{\"theme\":\"unknown\",\"reduce_motion\":true}', 0)", [])?; Ok(()) }).await.unwrap();
        assert_eq!(get(&db).await.unwrap(), AppearanceSettings::default());
        assert!(
            serde_json::from_str::<AppearanceSettings>(
                r#"{"theme":"dark","reduce_motion":true,"extra":1}"#
            )
            .is_err()
        );
    }
}
