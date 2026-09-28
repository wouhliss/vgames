//! Host-specific release choice from the public catalog. The choice is made
//! before fetching a signed manifest; the launch route is not executable data.

use vgames_proto::packages::{Platform, ReleaseInfo};

/// How a selected release would run on this host. Runtime and prerequisite
/// checks happen separately before installation or launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Native,
    WindowsEmulation,
    Rosetta,
    Proton,
    Wine { needs_rosetta: bool },
}

#[derive(Debug, Clone, Copy)]
pub struct SelectedRelease<'a> {
    pub release: &'a ReleaseInfo,
    pub route: Route,
}

const WINDOWS_X64: &[(Platform, Route)] = &[(Platform::WindowsX86_64, Route::Native)];
const WINDOWS_ARM64: &[(Platform, Route)] = &[
    (Platform::WindowsAarch64, Route::Native),
    (Platform::WindowsX86_64, Route::WindowsEmulation),
];
const LINUX_X64: &[(Platform, Route)] = &[
    (Platform::LinuxX86_64, Route::Native),
    (Platform::WindowsX86_64, Route::Proton),
];
const LINUX_ARM64: &[(Platform, Route)] = &[(Platform::LinuxAarch64, Route::Native)];
const MACOS_ARM64: &[(Platform, Route)] = &[
    (Platform::MacosAarch64, Route::Native),
    (Platform::MacosX86_64, Route::Rosetta),
    (
        Platform::WindowsX86_64,
        Route::Wine {
            needs_rosetta: true,
        },
    ),
];
const MACOS_X64: &[(Platform, Route)] = &[
    (Platform::MacosX86_64, Route::Native),
    (
        Platform::WindowsX86_64,
        Route::Wine {
            needs_rosetta: false,
        },
    ),
];

/// Returns the build to offer, independent of the server's list order. A
/// native build wins over a newer compatibility build. If malformed catalog
/// data contains multiple releases for one platform, the highest sequence wins.
pub fn select_release(releases: &[ReleaseInfo], host: Platform) -> Option<SelectedRelease<'_>> {
    let preferences = match host {
        Platform::WindowsX86_64 => WINDOWS_X64,
        Platform::WindowsAarch64 => WINDOWS_ARM64,
        Platform::LinuxX86_64 => LINUX_X64,
        Platform::LinuxAarch64 => LINUX_ARM64,
        Platform::MacosAarch64 => MACOS_ARM64,
        Platform::MacosX86_64 => MACOS_X64,
    };
    preferences.iter().find_map(|(platform, route)| {
        releases
            .iter()
            .filter(|release| release.platform == *platform && release.sequence > 0)
            .max_by_key(|release| (release.sequence, release.published_at, release.version_id))
            .map(|release| SelectedRelease {
                release,
                route: *route,
            })
    })
}

/// The host builds supported by the catalog selection rules.
pub fn host_platform() -> Option<Platform> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some(Platform::WindowsX86_64),
        ("windows", "aarch64") => Some(Platform::WindowsAarch64),
        ("linux", "x86_64") => Some(Platform::LinuxX86_64),
        ("linux", "aarch64") => Some(Platform::LinuxAarch64),
        ("macos", "x86_64") => Some(Platform::MacosX86_64),
        ("macos", "aarch64") => Some(Platform::MacosAarch64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;
    use uuid::Uuid;

    fn release(platform: Platform, sequence: i64) -> ReleaseInfo {
        ReleaseInfo {
            platform,
            version_id: Uuid::now_v7(),
            version_label: sequence.to_string(),
            sequence,
            total_size: 100,
            published_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn chooses_each_hosts_first_available_route() {
        let windows = release(Platform::WindowsX86_64, 9);
        let mac_x64 = release(Platform::MacosX86_64, 1);
        let linux = release(Platform::LinuxX86_64, 1);
        let releases = [windows, mac_x64, linux];
        let cases = [
            (
                Platform::WindowsX86_64,
                Platform::WindowsX86_64,
                Route::Native,
            ),
            (
                Platform::WindowsAarch64,
                Platform::WindowsX86_64,
                Route::WindowsEmulation,
            ),
            (Platform::LinuxX86_64, Platform::LinuxX86_64, Route::Native),
            (
                Platform::MacosAarch64,
                Platform::MacosX86_64,
                Route::Rosetta,
            ),
            (Platform::MacosX86_64, Platform::MacosX86_64, Route::Native),
        ];
        for (host, platform, route) in cases {
            let selected = select_release(&releases, host).unwrap();
            assert_eq!(selected.release.platform, platform);
            assert_eq!(selected.route, route);
        }
        assert!(select_release(&releases, Platform::LinuxAarch64).is_none());
    }

    #[test]
    fn falls_back_to_proton_or_wine_only_where_supported() {
        let releases = [release(Platform::WindowsX86_64, 1)];
        assert_eq!(
            select_release(&releases, Platform::LinuxX86_64)
                .unwrap()
                .route,
            Route::Proton
        );
        assert_eq!(
            select_release(&releases, Platform::MacosAarch64)
                .unwrap()
                .route,
            Route::Wine {
                needs_rosetta: true
            }
        );
        assert_eq!(
            select_release(&releases, Platform::MacosX86_64)
                .unwrap()
                .route,
            Route::Wine {
                needs_rosetta: false
            }
        );
        assert!(select_release(&releases, Platform::LinuxAarch64).is_none());
    }

    #[test]
    fn native_precedes_newer_fallback_and_latest_native_wins() {
        let releases = [
            release(Platform::WindowsX86_64, 100),
            release(Platform::LinuxX86_64, 1),
            release(Platform::LinuxX86_64, 3),
            release(Platform::LinuxX86_64, 0),
        ];
        let selected = select_release(&releases, Platform::LinuxX86_64).unwrap();
        assert_eq!(selected.route, Route::Native);
        assert_eq!(selected.release.sequence, 3);
        assert!(select_release(&[], Platform::LinuxX86_64).is_none());
    }
}
