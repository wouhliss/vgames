//! Host-specific release choice from the public catalog (09 §1). The choice is
//! made before fetching a signed manifest; the route only says how a build
//! would run, it is never executable data.

use vgames_core::manifest::Platform as Host;
use vgames_proto::packages::{Platform, ReleaseInfo};

/// The one host detection of the launcher.
pub use crate::launch::orchestrate::host_platform;

/// How a selected release would run on this host. Runtime and prerequisite
/// checks happen separately before installation or launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Native,
    /// x86-64 Windows build on Windows on Arm (OS emulation).
    WindowsEmulation,
    /// x86-64 macOS build on Apple silicon.
    Rosetta,
    Proton,
    Wine {
        needs_rosetta: bool,
    },
}

impl Route {
    /// The OS runs the build itself, with no compatibility runtime of ours.
    pub fn is_native(self) -> bool {
        matches!(self, Self::Native | Self::WindowsEmulation | Self::Rosetta)
    }
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

/// 09 §1, in order of preference.
pub fn preferences(host: Host) -> &'static [(Platform, Route)] {
    match host {
        Host::WindowsX86_64 => WINDOWS_X64,
        Host::WindowsAarch64 => WINDOWS_ARM64,
        Host::LinuxX86_64 => LINUX_X64,
        Host::LinuxAarch64 => LINUX_ARM64,
        Host::MacosAarch64 => MACOS_ARM64,
        Host::MacosX86_64 => MACOS_X64,
    }
}

/// The route a build of `platform` would take on `host`, if any.
pub fn route_for(platforms: &[Platform], host: Host) -> Option<(Platform, Route)> {
    preferences(host)
        .iter()
        .find(|(platform, _)| platforms.contains(platform))
        .copied()
}

/// Returns the build to offer, independent of the server's list order. A
/// native build wins over a newer compatibility build. If malformed catalog
/// data contains multiple releases for one platform, the highest sequence wins.
pub fn select_release(releases: &[ReleaseInfo], host: Host) -> Option<SelectedRelease<'_>> {
    preferences(host).iter().find_map(|(platform, route)| {
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

/// The catalog's platform as the manifest's (same wire names).
pub fn to_core(platform: Platform) -> Host {
    match platform {
        Platform::WindowsX86_64 => Host::WindowsX86_64,
        Platform::WindowsAarch64 => Host::WindowsAarch64,
        Platform::LinuxX86_64 => Host::LinuxX86_64,
        Platform::LinuxAarch64 => Host::LinuxAarch64,
        Platform::MacosAarch64 => Host::MacosAarch64,
        Platform::MacosX86_64 => Host::MacosX86_64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::orchestrate::runs_natively;
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

    /// Every host × set of available platforms of 09 §1: the first route of
    /// the host's preference list that the set offers, or nothing.
    #[test]
    fn whole_table_of_09_section_1() {
        use Platform::*;
        let wine = |needs_rosetta| Route::Wine { needs_rosetta };
        // (host, platform → expected route; absent = never chosen)
        let table: [(Host, &[(Platform, Route)]); 6] = [
            (Host::WindowsX86_64, &[(WindowsX86_64, Route::Native)]),
            (
                Host::WindowsAarch64,
                &[
                    (WindowsAarch64, Route::Native),
                    (WindowsX86_64, Route::WindowsEmulation),
                ],
            ),
            (
                Host::LinuxX86_64,
                &[(LinuxX86_64, Route::Native), (WindowsX86_64, Route::Proton)],
            ),
            (Host::LinuxAarch64, &[(LinuxAarch64, Route::Native)]),
            (
                Host::MacosAarch64,
                &[
                    (MacosAarch64, Route::Native),
                    (MacosX86_64, Route::Rosetta),
                    (WindowsX86_64, wine(true)),
                ],
            ),
            (
                Host::MacosX86_64,
                &[(MacosX86_64, Route::Native), (WindowsX86_64, wine(false))],
            ),
        ];
        for (host, expected) in table {
            // Every subset of the six platforms.
            for mask in 0u32..(1 << Platform::ALL.len()) {
                let offered: Vec<Platform> = Platform::ALL
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| mask & (1 << i) != 0)
                    .map(|(_, p)| p)
                    .collect();
                let releases: Vec<ReleaseInfo> = offered.iter().map(|p| release(*p, 1)).collect();
                let want = expected.iter().find(|(p, _)| offered.contains(p)).copied();
                let got = select_release(&releases, host).map(|s| (s.release.platform, s.route));
                assert_eq!(got, want, "host {host:?}, offered {offered:?}");
                assert_eq!(route_for(&offered, host), want);
            }
        }
    }

    /// One notion of "native": the route says so exactly where the launcher
    /// would start the build without a compatibility runtime.
    #[test]
    fn native_route_matches_runs_natively() {
        for host in Host::ALL {
            for platform in Platform::ALL {
                let releases = [release(platform, 1)];
                let native = select_release(&releases, host).is_some_and(|s| s.route.is_native());
                assert_eq!(
                    native,
                    runs_natively(to_core(platform), host),
                    "host {host:?}, build {platform:?}"
                );
            }
        }
    }

    #[test]
    fn native_precedes_newer_fallback_and_latest_native_wins() {
        let releases = [
            release(Platform::WindowsX86_64, 100),
            release(Platform::LinuxX86_64, 1),
            release(Platform::LinuxX86_64, 3),
            release(Platform::LinuxX86_64, 0),
        ];
        let selected = select_release(&releases, Host::LinuxX86_64).unwrap();
        assert_eq!(selected.route, Route::Native);
        assert_eq!(selected.release.sequence, 3);
        assert!(select_release(&[], Host::LinuxX86_64).is_none());
    }

    #[test]
    fn unpublished_sequences_are_ignored() {
        let releases = [release(Platform::LinuxX86_64, 0)];
        assert!(select_release(&releases, Host::LinuxX86_64).is_none());
    }

    #[test]
    fn platform_conversion_keeps_wire_names() {
        for platform in Platform::ALL {
            assert_eq!(platform.as_str(), to_core(platform).as_str());
        }
    }

    #[test]
    fn host_platform_is_known_on_supported_targets() {
        assert!(host_platform().is_some());
    }
}
