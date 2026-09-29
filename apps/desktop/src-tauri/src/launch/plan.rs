//! Launch plans: how a resolved target becomes a process (A2-T09). Compat
//! plans are built by the runtime manager (A2-T16/T17) from verified runtimes
//! and the signed compat profile; this module only assembles the command.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use vgames_core::compat::{Graphics, is_launcher_owned_env_key};
use vgames_core::manifest::{is_denied_env_key, is_valid_env_key};

use super::{ResolvedTarget, TargetError};

/// `GAMEID` when the compat profile names no umu id (umu's generic default).
const UMU_DEFAULT_GAME_ID: &str = "0";

/// Proton through umu-launcher (09-compatibility §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonPlan {
    /// The pinned `umu-run` from the runtime catalog.
    pub umu_run: PathBuf,
    /// The verified Proton build (`PROTONPATH`).
    pub proton_path: PathBuf,
    /// `<app data>/prefixes/<server>/<package>`.
    pub prefix: PathBuf,
    pub umu_game_id: Option<String>,
    /// `none`, or `steam` when the umu id comes from a Steam app id.
    pub store: String,
    /// Compat profile `env` plus local overrides, validated by `vgames_core::compat`.
    pub env: BTreeMap<String, String>,
    /// `WINEDLLOVERRIDES` (`CompatProfile::wine_dll_overrides`), may be empty.
    pub dll_overrides: String,
}

/// Wine with a Metal graphics backend on macOS (09-compatibility §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WinePlan {
    /// The pinned Wine binary from the runtime catalog.
    pub wine: PathBuf,
    pub prefix: PathBuf,
    pub graphics: Graphics,
    pub env: BTreeMap<String, String>,
    /// Backend and profile DLL overrides (`name=mode;…`), may be empty.
    pub dll_overrides: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchPlan {
    Native,
    Proton(ProtonPlan),
    Wine(WinePlan),
}

/// The final process description. Built only through [`LaunchPlan::prepare`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedLaunch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub working_dir: PathBuf,
    /// The complete environment: the child inherits nothing else.
    pub env: BTreeMap<String, OsString>,
    /// Variables the runner sets itself; [`Self::inject_env`] may not replace them.
    owned: Vec<String>,
}

impl LaunchPlan {
    /// Builds the process for `target`. `host_env` is the launcher's
    /// environment (`std::env::vars_os()`); only allowlisted variables pass.
    /// Later layers win: host, manifest `env`, compat `env`, runner variables.
    pub fn prepare(
        &self,
        target: ResolvedTarget,
        host_env: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Result<PreparedLaunch, TargetError> {
        let mut env: BTreeMap<String, OsString> = host_env
            .into_iter()
            .filter_map(|(k, v)| Some((fold_key(k.to_str()?), v)))
            .filter(|(k, _)| is_host_allowed(k))
            .collect();
        for (key, value) in &target.env {
            check_package_key(key)?;
            env.insert(fold_key(key), value.into());
        }
        let game = OsString::from(target.executable);
        let mut args: Vec<OsString> = target.args.into_iter().map(OsString::from).collect();
        let mut owned = BTreeMap::new();
        let program = match self {
            Self::Native => {
                return Ok(PreparedLaunch {
                    program: game.into(),
                    args,
                    working_dir: target.working_dir,
                    env,
                    owned: Vec::new(),
                });
            }
            Self::Proton(plan) => {
                add_compat_env(&mut env, &plan.env)?;
                owned.insert("WINEPREFIX", plan.prefix.clone().into_os_string());
                owned.insert("PROTONPATH", plan.proton_path.clone().into_os_string());
                let game_id = plan.umu_game_id.as_deref().unwrap_or(UMU_DEFAULT_GAME_ID);
                owned.insert("GAMEID", game_id.into());
                owned.insert("STORE", plan.store.as_str().into());
                if !plan.dll_overrides.is_empty() {
                    owned.insert("WINEDLLOVERRIDES", plan.dll_overrides.as_str().into());
                }
                plan.umu_run.clone()
            }
            Self::Wine(plan) => {
                add_compat_env(&mut env, &plan.env)?;
                owned.insert("WINEPREFIX", plan.prefix.clone().into_os_string());
                if !plan.dll_overrides.is_empty() {
                    owned.insert("WINEDLLOVERRIDES", plan.dll_overrides.as_str().into());
                }
                plan.wine.clone()
            }
        };
        // Compat runners take the Windows executable as their first argument.
        args.insert(0, game);
        env.extend(owned.iter().map(|(k, v)| (fold_key(k), v.clone())));
        Ok(PreparedLaunch {
            program,
            args,
            working_dir: target.working_dir,
            env,
            owned: owned.into_keys().map(fold_key).collect(),
        })
    }
}

impl PreparedLaunch {
    /// Extra variables from the launcher itself (Agent 4: overlay endpoint,
    /// Vulkan layer, GL preload). Never package or network data. Runner
    /// variables cannot be replaced.
    pub fn inject_env(&mut self, key: &str, value: impl Into<OsString>) -> Result<(), TargetError> {
        let folded = fold_key(key);
        if !is_valid_env_key(key) || self.owned.contains(&folded) {
            return Err(TargetError::EnvKey(key.to_owned()));
        }
        self.env.insert(folded, value.into());
        Ok(())
    }

    /// The command with explicit argv and a cleared environment. Spawning,
    /// suspension and tracking are the caller's job.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(&self.working_dir)
            .env_clear()
            .envs(&self.env);
        command
    }
}

fn check_package_key(key: &str) -> Result<(), TargetError> {
    // The manifest was validated already; checked again because this is the
    // last step before the value reaches a process.
    if is_valid_env_key(key) && !is_denied_env_key(key) {
        Ok(())
    } else {
        Err(TargetError::EnvKey(key.to_owned()))
    }
}

fn add_compat_env(
    env: &mut BTreeMap<String, OsString>,
    profile: &BTreeMap<String, String>,
) -> Result<(), TargetError> {
    for (key, value) in profile {
        check_package_key(key)?;
        if is_launcher_owned_env_key(key) {
            return Err(TargetError::EnvKey(key.clone()));
        }
        env.insert(fold_key(key), value.into());
    }
    Ok(())
}

/// Windows variable names are case-insensitive.
fn fold_key(key: &str) -> String {
    if cfg!(windows) {
        key.to_ascii_uppercase()
    } else {
        key.to_owned()
    }
}

/// Host variables a game may see: what it needs to find the user's session,
/// display, audio, locale and folders. Anything else (tokens, proxies, the
/// launcher's own settings, loader variables) is dropped.
fn is_host_allowed(key: &str) -> bool {
    #[cfg(windows)]
    const ALLOWED: &[&str] = &[
        "PATH",
        "PATHEXT",
        "SYSTEMROOT",
        "SYSTEMDRIVE",
        "WINDIR",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "USERNAME",
        "USERDOMAIN",
        "HOMEDRIVE",
        "HOMEPATH",
        "APPDATA",
        "LOCALAPPDATA",
        "PROGRAMDATA",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "PROGRAMW6432",
        "COMMONPROGRAMFILES",
        "COMMONPROGRAMFILES(X86)",
        "COMMONPROGRAMW6432",
        "PUBLIC",
        "ALLUSERSPROFILE",
        "COMPUTERNAME",
        "OS",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
        "PROCESSOR_IDENTIFIER",
        "PROCESSOR_LEVEL",
        "PROCESSOR_REVISION",
    ];
    #[cfg(target_os = "macos")]
    const ALLOWED: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "TMPDIR",
        "LANG",
        "LANGUAGE",
        "TZ",
        "__CF_USER_TEXT_ENCODING",
    ];
    #[cfg(not(any(windows, target_os = "macos")))]
    const ALLOWED: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "TMPDIR",
        "LANG",
        "LANGUAGE",
        "TZ",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XAUTHORITY",
        "XDG_RUNTIME_DIR",
        "XDG_SESSION_TYPE",
        "XDG_SESSION_DESKTOP",
        "XDG_CURRENT_DESKTOP",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "XDG_DATA_DIRS",
        "XDG_CONFIG_DIRS",
        "DBUS_SESSION_BUS_ADDRESS",
        "PULSE_SERVER",
        "PULSE_RUNTIME_PATH",
        "PIPEWIRE_RUNTIME_DIR",
    ];
    ALLOWED.contains(&key) || (cfg!(not(windows)) && key.starts_with("LC_"))
}

#[cfg(test)]
mod tests;
