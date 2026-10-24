//! Save base resolution (06 §1). Native bases come from the OS known-folder
//! APIs; compat launches resolve inside the prefix through `compat::save_base`
//! only. Nothing here creates a folder or trusts a path from IPC.
use std::path::{Path, PathBuf};

use vgames_core::compat::RunnerKind;
use vgames_core::manifest::{SaveBase, SaveLocation};

use super::scan::{Error, Location};
use crate::compat;

/// The user's known folders on this OS. `None` when the OS defines no such
/// folder (an unset XDG documents dir): that base is refused, never guessed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KnownFolders {
    pub home: Option<PathBuf>,
    pub documents: Option<PathBuf>,
    pub saved_games: Option<PathBuf>,
    pub appdata: Option<PathBuf>,
    pub localappdata: Option<PathBuf>,
    pub xdg_data: Option<PathBuf>,
    pub xdg_config: Option<PathBuf>,
}

impl KnownFolders {
    /// This user's folders, following the 06 §1 table for the build's OS.
    pub fn native() -> Self {
        let base = directories::BaseDirs::new();
        let user = directories::UserDirs::new();
        let home = base.as_ref().map(|dirs| dirs.home_dir().to_owned());
        let documents = user
            .as_ref()
            .and_then(|dirs| dirs.document_dir().map(Path::to_owned));
        // `config` is %APPDATA% (Roaming), $XDG_CONFIG_HOME or Application Support.
        let config = base.as_ref().map(|dirs| dirs.config_dir().to_owned());
        #[cfg(windows)]
        {
            let local = base.as_ref().map(|dirs| dirs.data_local_dir().to_owned());
            Self {
                home,
                documents,
                saved_games: windows::saved_games(),
                appdata: config.clone(),
                localappdata: local.clone(),
                xdg_data: local,
                xdg_config: config,
            }
        }
        #[cfg(not(windows))]
        {
            // $XDG_DATA_HOME on Linux, ~/Library/Application Support on macOS.
            let data = base.as_ref().map(|dirs| dirs.data_dir().to_owned());
            Self {
                home,
                documents,
                saved_games: data.clone(),
                appdata: config.clone(),
                localappdata: data.clone(),
                xdg_data: data,
                xdg_config: config,
            }
        }
    }

    fn get(&self, base: SaveBase) -> Option<&Path> {
        match base {
            SaveBase::Install => None,
            SaveBase::Home => self.home.as_deref(),
            SaveBase::Documents => self.documents.as_deref(),
            SaveBase::SavedGames => self.saved_games.as_deref(),
            SaveBase::Appdata => self.appdata.as_deref(),
            SaveBase::Localappdata => self.localappdata.as_deref(),
            SaveBase::XdgData => self.xdg_data.as_deref(),
            SaveBase::XdgConfig => self.xdg_config.as_deref(),
        }
    }
}

/// How the game runs: natively, or a Windows build inside a compat prefix.
#[derive(Clone, Debug)]
pub enum Target {
    Native,
    Compat { prefix: PathBuf, runner: RunnerKind },
}

/// The absolute base folder for one location. `install` is the verified
/// install root on every target (09 §6).
pub fn base_dir(
    base: SaveBase,
    folders: &KnownFolders,
    install: &Path,
    target: &Target,
) -> Result<PathBuf, Error> {
    let dir = match (base, target) {
        (SaveBase::Install, _) => install.to_owned(),
        (_, Target::Native) => folders.get(base).ok_or(Error::Invalid)?.to_owned(),
        (_, Target::Compat { prefix, runner }) => {
            compat::save_base(prefix, *runner, base).ok_or(Error::Invalid)?
        }
    };
    if !dir.is_absolute() {
        return Err(Error::Invalid);
    }
    Ok(dir)
}

/// Opens every location of a verified manifest's `saves.locations`. A missing
/// folder is an empty location (scanning creates nothing); an unresolvable base
/// refuses the whole set, so a partial snapshot is never taken or restored.
pub fn open_all(
    configs: &[SaveLocation],
    folders: &KnownFolders,
    install: &Path,
    target: &Target,
) -> Result<Vec<Location>, Error> {
    configs
        .iter()
        .map(|config| Location::open(config, &base_dir(config.base, folders, install, target)?))
        .collect()
}

#[cfg(windows)]
mod windows {
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;

    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_SavedGames, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
    };

    /// FOLDERID_SavedGames, which `directories` does not expose.
    pub fn saved_games() -> Option<PathBuf> {
        let mut raw: windows_sys::core::PWSTR = std::ptr::null_mut();
        #[allow(unsafe_code)]
        // SAFETY: valid GUID pointer, default flags, the current user's token
        // (null), and an out pointer the shell fills with a CoTaskMem string.
        let result = unsafe {
            SHGetKnownFolderPath(
                &FOLDERID_SavedGames,
                KF_FLAG_DEFAULT as u32,
                std::ptr::null_mut(),
                &mut raw,
            )
        };
        let path = if result >= 0 && !raw.is_null() {
            #[allow(unsafe_code)]
            // SAFETY: on success `raw` is a NUL-terminated UTF-16 string that
            // stays valid until freed below.
            let wide = unsafe {
                let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
                std::slice::from_raw_parts(raw, len)
            };
            Some(PathBuf::from(std::ffi::OsString::from_wide(wide)))
        } else {
            None
        };
        #[allow(unsafe_code)]
        // SAFETY: the shell allocated `raw` (or left it null, which is allowed)
        // and requires the caller to free it exactly once.
        unsafe {
            CoTaskMemFree(raw.cast());
        }
        path
    }
}

#[cfg(test)]
mod tests;
