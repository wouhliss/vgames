//! Native known folders and the single shared compatibility-prefix mapping.
//! Resolve once in Rust; commands accept packages and opaque sources, never paths.
use crate::launch::orchestrate::LaunchLayer;
use directories::{BaseDirs, UserDirs};
use std::path::{Path, PathBuf};
use vgames_core::{compat::RunnerKind, manifest::SaveBase};

pub struct KnownFolders {
    home: PathBuf,
    documents: Option<PathBuf>,
    local_data: PathBuf,
    config: PathBuf,
    saved_games: Option<PathBuf>,
}
impl KnownFolders {
    pub fn native() -> Option<Self> {
        let user = UserDirs::new()?;
        let base = BaseDirs::new()?;
        #[cfg(windows)]
        let saved_games = windows_saved_games();
        #[cfg(not(windows))]
        let saved_games = Some(base.data_dir().to_owned());
        Some(Self {
            home: user.home_dir().to_owned(),
            documents: user.document_dir().map(Path::to_owned),
            local_data: base.data_local_dir().to_owned(),
            config: base.config_dir().to_owned(),
            saved_games,
        })
    }
    pub fn resolve(
        &self,
        install: &Path,
        prefix: Option<&Path>,
        layer: LaunchLayer,
        base: SaveBase,
    ) -> Option<PathBuf> {
        if base == SaveBase::Install {
            return Some(install.to_owned());
        }
        if layer != LaunchLayer::Native {
            let runner = match layer {
                LaunchLayer::Proton => RunnerKind::Proton,
                LaunchLayer::Wine => RunnerKind::Wine,
                LaunchLayer::Native => return None,
            };
            return crate::compat::save_base(prefix?, runner, base);
        }
        Some(match base {
            SaveBase::Install => install.to_owned(),
            SaveBase::Home => self.home.clone(),
            SaveBase::Documents => self.documents.clone()?,
            SaveBase::SavedGames => self.saved_games.clone()?,
            SaveBase::Appdata | SaveBase::XdgConfig => self.config.clone(),
            SaveBase::Localappdata => self.local_data.clone(),
            SaveBase::XdgData => self.local_data.clone(),
        })
    }
}
#[cfg(windows)]
fn windows_saved_games() -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::ptr::NonNull;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_SavedGames, KF_FLAG_DONT_VERIFY, SHGetKnownFolderPath,
    };
    struct Owned(NonNull<u16>);
    impl Drop for Owned {
        fn drop(&mut self) {
            #[allow(unsafe_code)]
            // SAFETY: SHGetKnownFolderPath allocates the non-null result with
            // the COM allocator; release its single owned allocation once.
            unsafe {
                CoTaskMemFree(self.0.as_ptr().cast());
            }
        }
    }
    let mut pointer = std::ptr::null_mut();
    #[allow(unsafe_code)]
    // SAFETY: constant folder GUID, current-user token and initialized output
    // pointer. No CREATE flag is used, so resolution does not create a folder.
    let status = unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_SavedGames,
            KF_FLAG_DONT_VERIFY as u32,
            std::ptr::null_mut(),
            &mut pointer,
        )
    };
    let owned = Owned(NonNull::new(pointer)?);
    if status < 0 {
        return None;
    }
    let mut text = Vec::new();
    for index in 0..32_768 {
        #[allow(unsafe_code)]
        // SAFETY: the API promises a NUL-terminated UTF-16 allocation; copy
        // it while Owned lives and stop at the first terminator.
        let unit = unsafe { owned.0.as_ptr().add(index).read() };
        if unit == 0 {
            return Some(PathBuf::from(OsString::from_wide(&text)));
        }
        text.push(unit);
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    fn folders() -> KnownFolders {
        KnownFolders {
            home: "native/home".into(),
            documents: Some("native/documents".into()),
            local_data: "native/local".into(),
            config: "native/config".into(),
            saved_games: Some("native/saved".into()),
        }
    }
    #[test]
    fn native_locations_use_known_folders_without_reading_environment_strings() {
        let folders = folders();
        let install = Path::new("verified-install");
        for (base, expected) in [
            (SaveBase::Install, "verified-install"),
            (SaveBase::Home, "native/home"),
            (SaveBase::Documents, "native/documents"),
            (SaveBase::SavedGames, "native/saved"),
            (SaveBase::Appdata, "native/config"),
            (SaveBase::Localappdata, "native/local"),
            (SaveBase::XdgConfig, "native/config"),
        ] {
            assert_eq!(
                folders.resolve(install, None, LaunchLayer::Native, base),
                Some(PathBuf::from(expected))
            );
        }
        let expected = "native/local";
        assert_eq!(
            folders.resolve(install, None, LaunchLayer::Native, SaveBase::XdgData),
            Some(PathBuf::from(expected))
        );
    }
    #[test]
    fn compatibility_uses_the_shared_prefix_mapping_and_install_stays_verified() {
        let folders = folders();
        let prefix = Path::new("package-prefix");
        let install = Path::new("verified-install");
        assert_eq!(
            folders.resolve(
                install,
                Some(prefix),
                LaunchLayer::Proton,
                SaveBase::Documents
            ),
            crate::compat::save_base(prefix, RunnerKind::Proton, SaveBase::Documents)
        );
        assert_eq!(
            folders.resolve(install, None, LaunchLayer::Proton, SaveBase::Documents),
            None
        );
        assert_eq!(
            folders.resolve(
                install,
                Some(prefix),
                LaunchLayer::Proton,
                SaveBase::Install
            ),
            Some(install.to_owned())
        );
        let mut missing = folders;
        missing.documents = None;
        assert_eq!(
            missing.resolve(install, None, LaunchLayer::Native, SaveBase::Documents),
            None
        );
    }
    #[cfg(windows)]
    #[test]
    fn saved_games_uses_the_windows_known_folder_api() {
        assert!(windows_saved_games().is_some_and(|path| path.is_absolute()));
    }
}
