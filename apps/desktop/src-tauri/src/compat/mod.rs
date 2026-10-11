//! Compatibility runtimes (GAME, 09-compatibility). `prefix_dir` was built by
//! INS under README §1 ("need it, build it") with exactly the shared signature.

pub mod catalog;
pub mod d3d;
pub mod extract;
pub mod pe;
pub mod plan;
pub mod preflight;
pub mod profiles;
pub mod rosetta;
pub mod runtimes;

use std::path::{Path, PathBuf};

use vgames_core::compat::RunnerKind;
use vgames_core::manifest::SaveBase;

use crate::events::PackageRef;

/// The Proton/Wine prefix of a package: `<data_dir>/prefixes/<server_id>/<package_id>`
/// on every OS, where `data_dir` is `AppPaths::data_dir` (Tauri's app data
/// directory; no macOS special case). Both ids are UUIDs, so the path never
/// leaves `<data_dir>/prefixes`.
pub fn prefix_dir(data_dir: &Path, package: &PackageRef) -> PathBuf {
    data_dir
        .join("prefixes")
        .join(package.server_id.to_string())
        .join(package.package_id.to_string())
}

/// The Windows user inside a Proton prefix.
pub const PROTON_USER: &str = "steamuser";

/// Where a cloud-save base (06-cloud-saves §1) lives inside a compat prefix
/// (09-compatibility §6): `steamuser` for Proton, the Mac user for Wine. `None` for
/// `install`, which resolves as on a native launch. Only this function maps save
/// bases into prefixes.
pub fn save_base(prefix: &Path, runner: RunnerKind, base: SaveBase) -> Option<PathBuf> {
    let user = match runner {
        RunnerKind::Proton => PROTON_USER.to_owned(),
        RunnerKind::Wine => wine_user(),
    };
    let home = prefix.join("drive_c").join("users").join(user);
    let relative: &[&str] = match base {
        SaveBase::Install => return None,
        SaveBase::Home => &[],
        SaveBase::Documents => &["Documents"],
        SaveBase::SavedGames => &["Saved Games"],
        SaveBase::Appdata | SaveBase::XdgConfig => &["AppData", "Roaming"],
        SaveBase::Localappdata | SaveBase::XdgData => &["AppData", "Local"],
    };
    Some(relative.iter().fold(home, |path, part| path.join(part)))
}

/// The name Wine gives the prefix's user: the login name (`USER`, else `LOGNAME`),
/// as one path component.
fn wine_user() -> String {
    let name = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default();
    let safe = !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0']);
    if safe { name } else { "user".to_owned() }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    #[test]
    fn prefix_lives_under_the_app_data_directory() {
        let package = PackageRef {
            server_id: Uuid::from_u128(1),
            package_id: Uuid::from_u128(2),
        };
        #[cfg(windows)]
        let data = Path::new(r"C:\Users\sam\AppData\Roaming\app.vgames.launcher");
        #[cfg(target_os = "macos")]
        let data = Path::new("/Users/sam/Library/Application Support/app.vgames.launcher");
        #[cfg(all(unix, not(target_os = "macos")))]
        let data = Path::new("/home/sam/.local/share/app.vgames.launcher");
        let prefix = prefix_dir(data, &package);
        assert!(prefix.starts_with(data.join("prefixes")));
        let parts: Vec<_> = prefix
            .strip_prefix(data)
            .unwrap()
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            parts,
            [
                "prefixes",
                "00000000-0000-0000-0000-000000000001",
                "00000000-0000-0000-0000-000000000002"
            ]
        );
    }

    #[test]
    fn every_save_base_resolves_inside_the_prefix() {
        let prefix = Path::new("/data/prefixes/s/p");
        let user = prefix.join("drive_c/users/steamuser");
        let cases = [
            (SaveBase::Install, None),
            (SaveBase::Home, Some(user.clone())),
            (SaveBase::Documents, Some(user.join("Documents"))),
            (SaveBase::SavedGames, Some(user.join("Saved Games"))),
            (SaveBase::Appdata, Some(user.join("AppData/Roaming"))),
            (SaveBase::XdgConfig, Some(user.join("AppData/Roaming"))),
            (SaveBase::Localappdata, Some(user.join("AppData/Local"))),
            (SaveBase::XdgData, Some(user.join("AppData/Local"))),
        ];
        for (base, want) in cases {
            assert_eq!(
                save_base(prefix, RunnerKind::Proton, base),
                want,
                "{base:?}"
            );
        }
        let wine = save_base(prefix, RunnerKind::Wine, SaveBase::Documents).unwrap();
        assert!(wine.starts_with(prefix.join("drive_c/users")));
        assert!(wine.ends_with("Documents"));
        assert_eq!(wine.components().count(), prefix.components().count() + 4);
        assert_eq!(save_base(prefix, RunnerKind::Wine, SaveBase::Install), None);
    }
}
