//! Shared compatibility save bases (PLAY-06 fallback for GAME-05). Save clients
//! use this mapping rather than deriving a second Wine/Proton directory layout.
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use vgames_core::{compat::RunnerKind, manifest::SaveBase};

/// Resolve a signed save base inside the package's prefix. `install` belongs
/// to the verified native install root and is therefore deliberately absent.
pub fn save_base(prefix: &Path, runner: RunnerKind, base: SaveBase) -> Option<PathBuf> {
    if base == SaveBase::Install {
        return None;
    }
    let user = match runner {
        RunnerKind::Proton => OsString::from("steamuser"),
        RunnerKind::Wine => current_user()?,
    };
    with_user(prefix, base, &user)
}
fn with_user(prefix: &Path, base: SaveBase, user: &OsStr) -> Option<PathBuf> {
    let name = user.to_str()?;
    vgames_core::paths::validate_path(name).ok()?;
    if name.contains('/') {
        return None;
    }
    let home = prefix.join("drive_c").join("users").join(user);
    Some(match base {
        SaveBase::Install => return None,
        SaveBase::Home => home,
        SaveBase::Documents => home.join("Documents"),
        SaveBase::SavedGames => home.join("Saved Games"),
        SaveBase::Appdata | SaveBase::XdgConfig => home.join("AppData").join("Roaming"),
        SaveBase::Localappdata | SaveBase::XdgData => home.join("AppData").join("Local"),
    })
}
#[cfg(not(target_os = "macos"))]
fn current_user() -> Option<OsString> {
    None
}
#[cfg(target_os = "macos")]
fn current_user() -> Option<OsString> {
    use std::os::unix::ffi::OsStrExt;
    let mut row = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buffer = vec![0u8; 65_536];
    let mut result = std::ptr::null_mut();
    #[allow(unsafe_code)]
    // SAFETY: getpwuid_r receives initialized output storage and the exact
    // buffer size. Inspect the passwd only on success and copy its name while
    // its backing buffer lives. This queries the OS account, not an environment variable.
    unsafe {
        if libc::getpwuid_r(
            libc::getuid(),
            row.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        ) != 0
            || result.is_null()
        {
            return None;
        }
        let row = row.assume_init();
        if row.pw_name.is_null() {
            return None;
        }
        Some(OsStr::from_bytes(std::ffi::CStr::from_ptr(row.pw_name).to_bytes()).to_os_string())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_save_base_uses_the_windows_layout_for_proton_and_wine() {
        let prefix = Path::new("fixture-prefix");
        let cases = [
            (SaveBase::Home, ""),
            (SaveBase::Documents, "Documents"),
            (SaveBase::SavedGames, "Saved Games"),
            (SaveBase::Appdata, "AppData/Roaming"),
            (SaveBase::Localappdata, "AppData/Local"),
            (SaveBase::XdgData, "AppData/Local"),
            (SaveBase::XdgConfig, "AppData/Roaming"),
        ];
        for (base, suffix) in cases {
            assert_eq!(
                save_base(prefix, RunnerKind::Proton, base),
                Some(prefix.join("drive_c/users/steamuser").join(suffix))
            );
            assert_eq!(
                with_user(prefix, base, OsStr::new("fixture-mac-user")),
                Some(prefix.join("drive_c/users/fixture-mac-user").join(suffix))
            );
        }
        assert_eq!(
            save_base(prefix, RunnerKind::Proton, SaveBase::Install),
            None
        );
        assert_eq!(save_base(prefix, RunnerKind::Wine, SaveBase::Install), None);
    }
    #[test]
    fn account_names_cannot_escape_or_replace_the_prefix_path() {
        for user in [
            "",
            ".",
            "..",
            "../other",
            "/absolute",
            "C:\\outside",
            "a/b",
            "a\\b",
            "‥",
            "CON",
        ] {
            assert_eq!(
                with_user(
                    Path::new("fixture-prefix"),
                    SaveBase::Home,
                    OsStr::new(user)
                ),
                None
            );
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn wine_queries_the_native_account_without_assuming_the_home_folder_name() {
        let user = current_user().unwrap();
        let prefix = Path::new("fixture-prefix");
        assert_eq!(
            save_base(prefix, RunnerKind::Wine, SaveBase::Documents),
            with_user(prefix, SaveBase::Documents, &user)
        );
    }
}
