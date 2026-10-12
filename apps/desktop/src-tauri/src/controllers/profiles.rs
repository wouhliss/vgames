//! Controller settings and per-package snapshots on the database worker.
//! Validate before writes; a package row never changes another server/package
//! or the global setting. Existing rows keep their own Nintendo-label override.

use serde::{Deserialize, Serialize};
use specta::Type;

use super::decision::EmulationMode;
use super::mapping::{Profile, ProfileError};
use crate::db::rusqlite::{OptionalExtension, params};
use crate::db::{self, Db, DbError, settings};
use crate::events::PackageRef;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ControllerSettings {
    pub nintendo_labels: bool,
}

struct GlobalSettings;
impl settings::Setting for GlobalSettings {
    const KEY: &'static str = "controllers.settings";
    type Value = ControllerSettings;
    fn default_value() -> Self::Value {
        ControllerSettings::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ControllerProfile {
    pub mode: EmulationMode,
    pub mapping: Profile,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Database(#[from] DbError),
    #[error(transparent)]
    Profile(#[from] ProfileError),
    #[error("The stored controller mode is invalid.")]
    InvalidMode,
}

#[derive(Clone)]
pub struct Profiles {
    db: Db,
}
impl Profiles {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn settings_get(&self) -> Result<ControllerSettings, StoreError> {
        Ok(self
            .db
            .call(|conn| settings::get::<GlobalSettings>(conn))
            .await?)
    }

    pub async fn settings_set(
        &self,
        value: ControllerSettings,
    ) -> Result<ControllerSettings, StoreError> {
        self.db
            .call(move |conn| settings::set::<GlobalSettings>(conn, &value))
            .await?;
        Ok(value)
    }

    pub async fn get(&self, package: PackageRef) -> Result<ControllerProfile, StoreError> {
        let (raw, labels) = self.db.call(move |conn| {
            let raw: Option<(String, String)> = conn.query_row(
                "SELECT mode, profile FROM controller_profiles WHERE server_id = ?1 AND package_id = ?2",
                params![package.server_id.to_string(), package.package_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            let labels = settings::get::<GlobalSettings>(conn)?.nintendo_labels;
            Ok((raw, labels))
        }).await?;
        match raw {
            None => Ok(ControllerProfile {
                mapping: Profile {
                    nintendo_labels: labels,
                    ..Profile::default()
                },
                ..ControllerProfile::default()
            }),
            Some((mode, json)) => Ok(ControllerProfile {
                mode: match mode.as_str() {
                    "auto" => EmulationMode::Auto,
                    "always" => EmulationMode::Always,
                    "never" => EmulationMode::Never,
                    _ => return Err(StoreError::InvalidMode),
                },
                mapping: Profile::parse(json.as_bytes())?,
            }),
        }
    }

    pub async fn set(
        &self,
        package: PackageRef,
        value: ControllerProfile,
    ) -> Result<ControllerProfile, StoreError> {
        value.mapping.validate()?;
        // Serialize and run the same size/shape validation used for reads before
        // enqueueing the SQL write. Invalid updates leave the previous row intact.
        let json = serde_json::to_vec(&value.mapping).map_err(DbError::from)?;
        Profile::parse(&json)?;
        let json = String::from_utf8(json)
            .map_err(|_| ProfileError::Syntax("Invalid profile encoding.".into()))?;
        let mode = match value.mode {
            EmulationMode::Auto => "auto",
            EmulationMode::Always => "always",
            EmulationMode::Never => "never",
        };
        self.db.call(move |conn| {
            conn.execute(
                "INSERT INTO controller_profiles (server_id, package_id, mode, profile, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (server_id, package_id) DO UPDATE SET mode = excluded.mode,
                 profile = excluded.profile, updated_at = excluded.updated_at",
                params![package.server_id.to_string(), package.package_id.to_string(), mode, json, db::now_unix()],
            )?;
            Ok(())
        }).await?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controllers::mapping::{Button, Remap};
    use uuid::Uuid;

    #[tokio::test]
    async fn package_profiles_survive_restart_and_are_isolated_from_global_settings() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("launcher.sqlite");
        let db = Db::open(&path).unwrap();
        let profiles = Profiles::new(db.clone());
        let first = PackageRef {
            server_id: Uuid::new_v4(),
            package_id: Uuid::new_v4(),
        };
        let other_server = PackageRef {
            server_id: Uuid::new_v4(),
            package_id: first.package_id,
        };
        let other_package = PackageRef {
            server_id: first.server_id,
            package_id: Uuid::new_v4(),
        };
        assert_eq!(
            profiles.get(first).await.unwrap(),
            ControllerProfile::default()
        );
        profiles
            .settings_set(ControllerSettings {
                nintendo_labels: true,
            })
            .await
            .unwrap();
        assert!(profiles.get(first).await.unwrap().mapping.nintendo_labels);
        let mut snapshot = profiles.get(first).await.unwrap();
        snapshot.mode = EmulationMode::Always;
        snapshot.mapping.left_stick.deadzone = 24;
        snapshot.mapping.right_stick.invert_y = true;
        snapshot.mapping.remap.push(Remap {
            from: Button::South,
            to: None,
        });
        profiles.set(first, snapshot.clone()).await.unwrap();
        profiles
            .settings_set(ControllerSettings::default())
            .await
            .unwrap();
        assert_eq!(profiles.get(first).await.unwrap(), snapshot);
        assert_eq!(
            profiles.get(other_server).await.unwrap(),
            ControllerProfile::default()
        );
        assert_eq!(
            profiles.get(other_package).await.unwrap(),
            ControllerProfile::default()
        );
        drop(profiles);
        drop(db);
        let reopened = Profiles::new(Db::open(&path).unwrap());
        assert_eq!(reopened.get(first).await.unwrap(), snapshot);
        assert_eq!(
            reopened.settings_get().await.unwrap(),
            ControllerSettings::default()
        );
    }

    #[tokio::test]
    async fn invalid_profiles_never_replace_an_existing_row() {
        let root = tempfile::tempdir().unwrap();
        let profiles = Profiles::new(Db::open(&root.path().join("launcher.sqlite")).unwrap());
        let package = PackageRef {
            server_id: Uuid::new_v4(),
            package_id: Uuid::new_v4(),
        };
        let original = ControllerProfile {
            mode: EmulationMode::Never,
            ..ControllerProfile::default()
        };
        profiles.set(package, original.clone()).await.unwrap();
        let mut invalid = original.clone();
        invalid.mapping.left_stick.deadzone = 100;
        assert!(matches!(
            profiles.set(package, invalid).await,
            Err(StoreError::Profile(ProfileError::Range))
        ));
        let mut invalid = original.clone();
        invalid.mapping.remap = vec![
            Remap {
                from: Button::South,
                to: None
            };
            2
        ];
        assert!(matches!(
            profiles.set(package, invalid).await,
            Err(StoreError::Profile(ProfileError::DuplicateRemap(_)))
        ));
        assert_eq!(profiles.get(package).await.unwrap(), original);
    }

    #[tokio::test]
    async fn corrupt_stored_profiles_fail_closed_without_writing_a_default() {
        let root = tempfile::tempdir().unwrap();
        let db = Db::open(&root.path().join("launcher.sqlite")).unwrap();
        let profiles = Profiles::new(db.clone());
        let package = PackageRef {
            server_id: Uuid::new_v4(),
            package_id: Uuid::new_v4(),
        };
        profiles
            .set(package, ControllerProfile::default())
            .await
            .unwrap();
        let server = package.server_id.to_string();
        let pkg = package.package_id.to_string();
        db.call(move |conn| {
            conn.execute("UPDATE controller_profiles SET profile = ?1 WHERE server_id = ?2 AND package_id = ?3",
                params![r#"{"left_stick":{"deadzone":100}}"#, server, pkg])?;
            Ok(())
        }).await.unwrap();
        assert!(matches!(
            profiles.get(package).await,
            Err(StoreError::Profile(ProfileError::Range))
        ));
        assert!(matches!(
            profiles.get(package).await,
            Err(StoreError::Profile(ProfileError::Range))
        ));
    }
}
