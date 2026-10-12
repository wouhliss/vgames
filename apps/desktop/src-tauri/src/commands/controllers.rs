//! Controller preferences/profile commands. Typed inputs are validated by the
//! profile store before any SQLite write; no work runs on the WebView thread.

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::State;

use crate::controllers::profiles::{
    ControllerProfile as StoredProfile, ControllerSettings, Profiles, StoreError,
};
use crate::error::AppError;
use crate::events::PackageRef;
use crate::state::AppState;

// Stored/imported mappings accept omitted fields for old database rows. IPC
// carries complete profiles, matching the UI contract and avoiding optional
// output fields in generated bindings.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ControllerProfile {
    pub mode: crate::controllers::decision::EmulationMode,
    pub mapping: ControllerMapping,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ControllerMapping {
    pub nintendo_labels: bool,
    pub left_stick: ControllerStickSettings,
    pub right_stick: ControllerStickSettings,
    pub remap: Vec<crate::controllers::mapping::Remap>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ControllerStickSettings {
    pub deadzone: u8,
    pub anti_deadzone: u8,
    pub invert_y: bool,
}

impl From<crate::controllers::mapping::StickSettings> for ControllerStickSettings {
    fn from(value: crate::controllers::mapping::StickSettings) -> Self {
        Self {
            deadzone: value.deadzone,
            anti_deadzone: value.anti_deadzone,
            invert_y: value.invert_y,
        }
    }
}
impl From<ControllerStickSettings> for crate::controllers::mapping::StickSettings {
    fn from(value: ControllerStickSettings) -> Self {
        Self {
            deadzone: value.deadzone,
            anti_deadzone: value.anti_deadzone,
            invert_y: value.invert_y,
        }
    }
}
impl From<StoredProfile> for ControllerProfile {
    fn from(value: StoredProfile) -> Self {
        Self {
            mode: value.mode,
            mapping: ControllerMapping {
                nintendo_labels: value.mapping.nintendo_labels,
                left_stick: value.mapping.left_stick.into(),
                right_stick: value.mapping.right_stick.into(),
                remap: value.mapping.remap,
            },
        }
    }
}
impl From<ControllerProfile> for StoredProfile {
    fn from(value: ControllerProfile) -> Self {
        Self {
            mode: value.mode,
            mapping: crate::controllers::mapping::Profile {
                nintendo_labels: value.mapping.nintendo_labels,
                left_stick: value.mapping.left_stick.into(),
                right_stick: value.mapping.right_stick.into(),
                remap: value.mapping.remap,
            },
        }
    }
}

#[tauri::command]
#[specta::specta]
pub async fn controllers_settings_get(
    state: State<'_, AppState>,
) -> Result<ControllerSettings, AppError> {
    Profiles::new(state.db.clone())
        .settings_get()
        .await
        .map_err(|error| AppError::internal("read controller settings", &error))
}

#[tauri::command]
#[specta::specta]
pub async fn controllers_settings_set(
    state: State<'_, AppState>,
    settings: ControllerSettings,
) -> Result<ControllerSettings, AppError> {
    Profiles::new(state.db.clone())
        .settings_set(settings)
        .await
        .map_err(|error| AppError::internal("save controller settings", &error))
}

#[tauri::command]
#[specta::specta]
pub async fn controllers_profile_get(
    state: State<'_, AppState>,
    package_ref: PackageRef,
) -> Result<ControllerProfile, AppError> {
    Profiles::new(state.db.clone())
        .get(package_ref)
        .await
        .map(ControllerProfile::from)
        .map_err(|error| AppError::internal("read the controller profile", &error))
}

#[tauri::command]
#[specta::specta]
pub async fn controllers_profile_set(
    state: State<'_, AppState>,
    package_ref: PackageRef,
    profile: ControllerProfile,
) -> Result<ControllerProfile, AppError> {
    Profiles::new(state.db.clone())
        .set(package_ref, profile.into())
        .await
        .map(ControllerProfile::from)
        .map_err(profile_write_error)
}

fn profile_write_error(error: StoreError) -> AppError {
    match error {
        StoreError::Profile(error) => AppError::invalid("profile", error.to_string()),
        other => AppError::internal("save the controller profile", &other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controllers::mapping::ProfileError;

    #[test]
    fn profile_ipc_carries_complete_defaults_and_refuses_partial_mappings() {
        let wire = ControllerProfile::from(StoredProfile::default());
        let json = serde_json::to_value(wire).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "mode": "auto", "mapping": {
                    "nintendo_labels": false,
                    "left_stick": {"deadzone": 8, "anti_deadzone": 0, "invert_y": false},
                    "right_stick": {"deadzone": 8, "anti_deadzone": 0, "invert_y": false},
                    "remap": [],
                }
            })
        );
        assert!(
            serde_json::from_value::<ControllerProfile>(serde_json::json!({
                "mode": "auto", "mapping": {}
            }))
            .is_err()
        );
        let mut unknown = json;
        unknown["mapping"]["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ControllerProfile>(unknown).is_err());
    }

    #[test]
    fn validation_failures_have_a_typed_profile_error() {
        assert!(
            matches!(profile_write_error(StoreError::Profile(ProfileError::Range)),
            AppError::InvalidInput { field, .. } if field == "profile")
        );
        let serialized = serde_json::to_value(profile_write_error(StoreError::Profile(
            ProfileError::Range,
        )))
        .unwrap();
        assert_eq!(serialized["kind"], "invalid_input");
    }
}
