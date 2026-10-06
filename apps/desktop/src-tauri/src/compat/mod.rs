//! Compatibility runtimes (GAME). Built by INS under README §1 ("need it,
//! build it") with exactly the shared signature; GAME owns and extends it.

use std::path::{Path, PathBuf};

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
}
