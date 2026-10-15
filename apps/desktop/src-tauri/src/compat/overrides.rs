//! A player's compatibility settings (GAME-07, 09-compatibility §4): the default
//! runner and per-package local overrides (runner version, graphics backend, extra
//! environment). Validated here before they are stored, and again where a launch
//! plan uses them; launcher-owned keys never come from an override.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use specta::Type;
use vgames_core::compat::{Graphics, is_launcher_owned_env_key};
use vgames_core::manifest::{is_denied_env_key, is_valid_env_key};
use vgames_core::runtimes::RuntimeId as CoreRuntimeId;

use crate::db::settings::{self, Setting};
use crate::db::{Db, DbError, now_unix};
use crate::events::PackageRef;

/// Extra variables per override.
pub const MAX_ENV: usize = 32;
/// Characters per value.
pub const MAX_ENV_VALUE: usize = 1024;

/// A runtime of the runtime catalog (09 §5), as the UI names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeId {
    UmuProton,
    GeProton,
    UmuLauncher,
    WineMacos,
    D3dmetal,
    Dxmt,
    DxvkMacos,
    Moltenvk,
}

impl From<CoreRuntimeId> for RuntimeId {
    fn from(id: CoreRuntimeId) -> Self {
        match id {
            CoreRuntimeId::UmuProton => Self::UmuProton,
            CoreRuntimeId::GeProton => Self::GeProton,
            CoreRuntimeId::UmuLauncher => Self::UmuLauncher,
            CoreRuntimeId::WineMacos => Self::WineMacos,
            CoreRuntimeId::D3dmetal => Self::D3dmetal,
            CoreRuntimeId::Dxmt => Self::Dxmt,
            CoreRuntimeId::DxvkMacos => Self::DxvkMacos,
            CoreRuntimeId::Moltenvk => Self::Moltenvk,
        }
    }
}

impl From<RuntimeId> for CoreRuntimeId {
    fn from(id: RuntimeId) -> Self {
        match id {
            RuntimeId::UmuProton => Self::UmuProton,
            RuntimeId::GeProton => Self::GeProton,
            RuntimeId::UmuLauncher => Self::UmuLauncher,
            RuntimeId::WineMacos => Self::WineMacos,
            RuntimeId::D3dmetal => Self::D3dmetal,
            RuntimeId::Dxmt => Self::Dxmt,
            RuntimeId::DxvkMacos => Self::DxvkMacos,
            RuntimeId::Moltenvk => Self::Moltenvk,
        }
    }
}

impl RuntimeId {
    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
    }

    pub fn as_str(self) -> &'static str {
        CoreRuntimeId::from(self).as_str()
    }
}

/// A Proton (Linux) or Wine (macOS) build from the runtime catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RunnerVersion {
    pub runtime: RuntimeId,
    pub version: String,
}

/// macOS graphics backends (09 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsBackend {
    D3dmetal,
    Dxmt,
    Dxvk,
    Wined3d,
}

impl From<GraphicsBackend> for Graphics {
    fn from(g: GraphicsBackend) -> Self {
        match g {
            GraphicsBackend::D3dmetal => Self::D3dmetal,
            GraphicsBackend::Dxmt => Self::Dxmt,
            GraphicsBackend::Dxvk => Self::Dxvk,
            GraphicsBackend::Wined3d => Self::Wined3d,
        }
    }
}

impl From<Graphics> for GraphicsBackend {
    fn from(g: Graphics) -> Self {
        match g {
            Graphics::D3dmetal => Self::D3dmetal,
            Graphics::Dxmt => Self::Dxmt,
            Graphics::Dxvk => Self::Dxvk,
            Graphics::Wined3d => Self::Wined3d,
        }
    }
}

/// A player's local override for one package.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CompatOverride {
    /// `None` = the default runner.
    pub runner: Option<RunnerVersion>,
    /// macOS only; `None` = the compat profile's order.
    pub graphics: Option<GraphicsBackend>,
    /// Extra environment variables, added after the compat profile's.
    pub env: BTreeMap<String, String>,
}

/// Why an environment entry was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum EnvRefusal {
    /// The key isn't `^[A-Z_][A-Z0-9_]{0,63}$` or the value isn't one short line.
    Format,
    /// The key is reserved (manifest denylist or launcher-owned).
    Denied,
}

/// What saving compatibility settings can report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompatSettingsError {
    #[error("not found")]
    NotFound,
    #[error("not a runner from the catalog for this computer")]
    UnknownRunner,
    #[error("the graphics backend is not available here")]
    GraphicsUnavailable { backend: GraphicsBackend },
    #[error("environment variable {key} is refused")]
    InvalidEnv { key: String, reason: EnvRefusal },
    #[error("{detail}")]
    Io { detail: String },
}

impl From<DbError> for CompatSettingsError {
    fn from(error: DbError) -> Self {
        tracing::error!(%error, "compat settings storage");
        Self::Io {
            detail: "Cannot save the compatibility settings.".into(),
        }
    }
}

/// Keys neither a compat profile nor a player override may set: the launcher-owned
/// ones (09 §2, §3) plus Proton's logging switches, which would turn on verbose
/// logs and choose where they are written (GAME-14 threat review C3). A package's
/// own manifest `env` may still set the logging switches.
pub fn is_compat_reserved_env_key(key: &str) -> bool {
    is_launcher_owned_env_key(key) || matches!(key, "PROTON_LOG" | "PROTON_LOG_DIR")
}

/// Checks one environment entry.
pub fn check_env(key: &str, value: &str) -> Result<(), CompatSettingsError> {
    let refusal = if !is_valid_env_key(key)
        || value.chars().count() > MAX_ENV_VALUE
        || value.contains(['\n', '\r', '\0'])
    {
        Some(EnvRefusal::Format)
    } else if is_denied_env_key(key) || is_compat_reserved_env_key(key) {
        Some(EnvRefusal::Denied)
    } else {
        None
    };
    match refusal {
        Some(reason) => Err(CompatSettingsError::InvalidEnv {
            key: key.to_owned(),
            reason,
        }),
        None => Ok(()),
    }
}

/// Checks the shape of an override (the runner and backend are checked against the
/// catalog by the caller).
pub fn check(over: &CompatOverride) -> Result<(), CompatSettingsError> {
    if over.env.len() > MAX_ENV {
        return Err(CompatSettingsError::InvalidEnv {
            key: String::new(),
            reason: EnvRefusal::Format,
        });
    }
    for (key, value) in &over.env {
        check_env(key, value)?;
    }
    if over.graphics == Some(GraphicsBackend::D3dmetal) {
        // Deferred in v1 (09 §3, §7).
        return Err(CompatSettingsError::GraphicsUnavailable {
            backend: GraphicsBackend::D3dmetal,
        });
    }
    Ok(())
}

/// The runner every package uses unless it overrides it (`None` = automatic).
pub struct DefaultRunner;
impl Setting for DefaultRunner {
    const KEY: &'static str = "compat.default_runner";
    type Value = Option<RunnerVersion>;
    fn default_value() -> Option<RunnerVersion> {
        None
    }
}

pub async fn default_runner(db: &Db) -> Result<Option<RunnerVersion>, DbError> {
    db.call(|conn| settings::get::<DefaultRunner>(conn)).await
}

pub async fn set_default_runner(db: &Db, runner: Option<RunnerVersion>) -> Result<(), DbError> {
    db.call(move |conn| settings::set::<DefaultRunner>(conn, &runner))
        .await
}

/// `runner_runtime, runner_version, graphics, env` as stored.
type StoredRow = (Option<String>, Option<String>, Option<String>, String);

pub async fn get(db: &Db, package: PackageRef) -> Result<Option<CompatOverride>, DbError> {
    let ids = (
        package.server_id.to_string(),
        package.package_id.to_string(),
    );
    db.call(move |conn| {
        let row: Option<StoredRow> = conn
            .query_row(
                "SELECT runner_runtime, runner_version, graphics, env FROM compat_overrides
                  WHERE server_id = ?1 AND package_id = ?2",
                params![ids.0, ids.1],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        Ok(row.map(|(runtime, version, graphics, env)| {
            let runner = match (runtime.as_deref().and_then(RuntimeId::parse), version) {
                (Some(runtime), Some(version)) => Some(RunnerVersion { runtime, version }),
                _ => None,
            };
            let graphics =
                graphics.and_then(|g| serde_json::from_value(serde_json::Value::String(g)).ok());
            // Stored values were checked; anything unreadable or no longer valid is dropped.
            let env: BTreeMap<String, String> = serde_json::from_str(&env).unwrap_or_default();
            let env = env
                .into_iter()
                .filter(|(k, v)| check_env(k, v).is_ok())
                .collect();
            CompatOverride {
                runner,
                graphics,
                env,
            }
        }))
    })
    .await
}

/// Stores an override that passed [`check`] and the caller's catalog checks.
pub async fn set(db: &Db, package: PackageRef, over: &CompatOverride) -> Result<(), DbError> {
    let ids = (
        package.server_id.to_string(),
        package.package_id.to_string(),
    );
    let runtime = over.runner.as_ref().map(|r| r.runtime.as_str());
    let version = over.runner.as_ref().map(|r| r.version.clone());
    let graphics = over.graphics.map(|g| match g {
        GraphicsBackend::D3dmetal => "d3dmetal",
        GraphicsBackend::Dxmt => "dxmt",
        GraphicsBackend::Dxvk => "dxvk",
        GraphicsBackend::Wined3d => "wined3d",
    });
    let env = serde_json::to_string(&over.env)?;
    db.call(move |conn| {
        conn.execute(
            "INSERT INTO compat_overrides
                 (server_id, package_id, runner_runtime, runner_version, graphics, env, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (server_id, package_id) DO UPDATE SET
                 runner_runtime = excluded.runner_runtime, runner_version = excluded.runner_version,
                 graphics = excluded.graphics, env = excluded.env, updated_at = excluded.updated_at",
            params![ids.0, ids.1, runtime, version, graphics, env, now_unix()],
        )?;
        Ok(())
    })
    .await
}

pub async fn reset(db: &Db, package: PackageRef) -> Result<(), DbError> {
    let ids = (
        package.server_id.to_string(),
        package.package_id.to_string(),
    );
    db.call(move |conn| {
        conn.execute(
            "DELETE FROM compat_overrides WHERE server_id = ?1 AND package_id = ?2",
            params![ids.0, ids.1],
        )?;
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    const PACKAGE: PackageRef = PackageRef {
        server_id: Uuid::from_u128(1),
        package_id: Uuid::from_u128(2),
    };

    #[test]
    fn env_rules() {
        assert_eq!(check_env("DXVK_HUD", "fps"), Ok(()));
        for (key, value, reason) in [
            ("dxvk_hud", "1", EnvRefusal::Format),
            ("A B", "1", EnvRefusal::Format),
            ("OK", "two\nlines", EnvRefusal::Format),
            ("PATH", "/x", EnvRefusal::Denied),
            ("LD_PRELOAD", "/x", EnvRefusal::Denied),
            ("WINEPREFIX", "/x", EnvRefusal::Denied),
            ("PROTONPATH", "/x", EnvRefusal::Denied),
            ("STEAM_COMPAT_DATA_PATH", "/x", EnvRefusal::Denied),
            ("PROTON_LOG", "1", EnvRefusal::Denied),
            ("PROTON_LOG_DIR", "/x", EnvRefusal::Denied),
        ] {
            assert_eq!(
                check_env(key, value),
                Err(CompatSettingsError::InvalidEnv {
                    key: key.into(),
                    reason
                }),
                "{key}"
            );
        }
        assert!(check_env("OK", &"x".repeat(MAX_ENV_VALUE + 1)).is_err());
    }

    #[test]
    fn d3dmetal_is_never_accepted() {
        let over = CompatOverride {
            graphics: Some(GraphicsBackend::D3dmetal),
            ..CompatOverride::default()
        };
        assert_eq!(
            check(&over),
            Err(CompatSettingsError::GraphicsUnavailable {
                backend: GraphicsBackend::D3dmetal
            })
        );
    }

    #[tokio::test]
    async fn overrides_and_the_default_runner_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher.db");
        let over = CompatOverride {
            runner: Some(RunnerVersion {
                runtime: RuntimeId::GeProton,
                version: "GE-Proton10-17".into(),
            }),
            graphics: Some(GraphicsBackend::Dxvk),
            env: [("DXVK_HUD".to_owned(), "fps".to_owned())].into(),
        };
        let runner = RunnerVersion {
            runtime: RuntimeId::UmuProton,
            version: "UMU-Proton-9.0-4e".into(),
        };
        {
            let db = Db::open(&path).unwrap();
            set(&db, PACKAGE, &over).await.unwrap();
            set_default_runner(&db, Some(runner.clone())).await.unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(get(&db, PACKAGE).await.unwrap(), Some(over));
        assert_eq!(default_runner(&db).await.unwrap(), Some(runner));
        reset(&db, PACKAGE).await.unwrap();
        assert_eq!(get(&db, PACKAGE).await.unwrap(), None);
        set_default_runner(&db, None).await.unwrap();
        assert_eq!(default_runner(&db).await.unwrap(), None);
    }
}
