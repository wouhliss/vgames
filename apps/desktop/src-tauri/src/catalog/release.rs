//! Which build of a package this computer installs (09-compatibility §1). The choice is made from the
//! public catalog before any signed manifest is fetched; the route only feeds badges and install plans,
//! never anything that runs.

use vgames_core::manifest::Platform;
use vgames_proto::packages::{Platform as WirePlatform, ReleaseInfo};

/// The platform this launcher runs on (one definition, shared with the launch path).
pub use crate::launch::orchestrate::host_platform;

/// How a selected build would run here. Runtime and prerequisite checks (Proton, Wine, Rosetta 2) happen
/// separately, before installing or launching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Native,
    /// An x86_64 Windows build on Windows on Arm, through the OS's own emulation.
    WindowsEmulation,
    /// An Intel macOS build on Apple silicon, through Rosetta 2.
    Rosetta,
    Proton,
    Wine {
        needs_rosetta: bool,
    },
}

impl Route {
    /// True when the build runs without a compatibility layer (the OS's own x86 emulation counts), exactly
    /// where `launch::orchestrate::runs_natively` says so.
    pub const fn is_native(self) -> bool {
        matches!(
            self,
            Route::Native | Route::WindowsEmulation | Route::Rosetta
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SelectedRelease<'a> {
    pub release: &'a ReleaseInfo,
    pub route: Route,
}

/// The builds each host accepts, best first (09 §1).
const fn preferences(host: Platform) -> &'static [(Platform, Route)] {
    use Platform::*;
    match host {
        WindowsX86_64 => &[(WindowsX86_64, Route::Native)],
        WindowsAarch64 => &[
            (WindowsAarch64, Route::Native),
            (WindowsX86_64, Route::WindowsEmulation),
        ],
        LinuxX86_64 => &[(LinuxX86_64, Route::Native), (WindowsX86_64, Route::Proton)],
        LinuxAarch64 => &[(LinuxAarch64, Route::Native)],
        MacosAarch64 => &[
            (MacosAarch64, Route::Native),
            (MacosX86_64, Route::Rosetta),
            (
                WindowsX86_64,
                Route::Wine {
                    needs_rosetta: true,
                },
            ),
        ],
        MacosX86_64 => &[
            (MacosX86_64, Route::Native),
            (
                WindowsX86_64,
                Route::Wine {
                    needs_rosetta: false,
                },
            ),
        ],
    }
}

/// The same target on the wire (`vgames-proto`) and in manifests (`vgames-core`).
const fn wire(platform: Platform) -> WirePlatform {
    match platform {
        Platform::WindowsX86_64 => WirePlatform::WindowsX86_64,
        Platform::WindowsAarch64 => WirePlatform::WindowsAarch64,
        Platform::LinuxX86_64 => WirePlatform::LinuxX86_64,
        Platform::LinuxAarch64 => WirePlatform::LinuxAarch64,
        Platform::MacosAarch64 => WirePlatform::MacosAarch64,
        Platform::MacosX86_64 => WirePlatform::MacosX86_64,
    }
}

/// The build to offer, whatever the server's list order: a native build wins over a newer compatibility
/// build. If the catalog holds several releases for one platform, the highest sequence wins; releases with
/// a non-positive sequence are ignored.
pub fn select_release(releases: &[ReleaseInfo], host: Platform) -> Option<SelectedRelease<'_>> {
    preferences(host).iter().find_map(|&(platform, route)| {
        releases
            .iter()
            .filter(|release| release.platform == wire(platform) && release.sequence > 0)
            .max_by_key(|release| (release.sequence, release.published_at, release.version_id))
            .map(|release| SelectedRelease { release, route })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::orchestrate::runs_natively;
    use time::OffsetDateTime;
    use uuid::Uuid;

    fn release(platform: Platform, sequence: i64) -> ReleaseInfo {
        ReleaseInfo {
            platform: wire(platform),
            version_id: Uuid::now_v7(),
            version_label: sequence.to_string(),
            sequence,
            total_size: 100,
            published_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// 09-compatibility §1, written out independently of `preferences`.
    fn table(host: Platform) -> Vec<(Platform, Route)> {
        use Platform::*;
        match host {
            WindowsX86_64 => vec![(WindowsX86_64, Route::Native)],
            WindowsAarch64 => vec![
                (WindowsAarch64, Route::Native),
                (WindowsX86_64, Route::WindowsEmulation),
            ],
            LinuxX86_64 => vec![(LinuxX86_64, Route::Native), (WindowsX86_64, Route::Proton)],
            LinuxAarch64 => vec![(LinuxAarch64, Route::Native)],
            MacosAarch64 => vec![
                (MacosAarch64, Route::Native),
                (MacosX86_64, Route::Rosetta),
                (
                    WindowsX86_64,
                    Route::Wine {
                        needs_rosetta: true,
                    },
                ),
            ],
            MacosX86_64 => vec![
                (MacosX86_64, Route::Native),
                (
                    WindowsX86_64,
                    Route::Wine {
                        needs_rosetta: false,
                    },
                ),
            ],
        }
    }

    #[test]
    fn every_host_picks_its_best_build_from_every_set_of_platforms() {
        for host in Platform::ALL {
            for mask in 0u32..(1 << Platform::ALL.len()) {
                let available: Vec<Platform> = Platform::ALL
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| mask & (1 << i) != 0)
                    .map(|(_, p)| p)
                    .collect();
                let releases: Vec<ReleaseInfo> = available.iter().map(|&p| release(p, 1)).collect();
                let expected = table(host).into_iter().find(|(p, _)| available.contains(p));
                let selected =
                    select_release(&releases, host).map(|s| (s.release.platform, s.route));
                assert_eq!(
                    selected,
                    expected.map(|(p, route)| (wire(p), route)),
                    "host {host:?}, available {available:?}"
                );
            }
        }
    }

    #[test]
    fn the_native_route_is_exactly_where_the_launcher_runs_natively() {
        for host in Platform::ALL {
            for platform in Platform::ALL {
                let releases = [release(platform, 1)];
                let selected = select_release(&releases, host);
                if runs_natively(platform, host) {
                    let selected = selected.unwrap_or_else(|| {
                        panic!("{platform:?} runs natively on {host:?} but is not offered")
                    });
                    assert!(selected.route.is_native(), "{platform:?} on {host:?}");
                } else if let Some(selected) = selected {
                    assert!(
                        !selected.route.is_native(),
                        "{platform:?} on {host:?} is offered as native"
                    );
                }
            }
        }
    }

    #[test]
    fn a_native_build_wins_over_a_newer_compatibility_build_and_the_latest_native_wins() {
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

    #[test]
    fn releases_without_a_positive_sequence_are_never_offered() {
        let releases = [
            release(Platform::WindowsX86_64, 0),
            release(Platform::WindowsX86_64, -1),
        ];
        assert!(select_release(&releases, Platform::WindowsX86_64).is_none());
    }

    #[test]
    fn manifest_and_wire_platforms_name_the_same_targets() {
        for platform in Platform::ALL {
            assert_eq!(wire(platform).as_str(), platform.as_str());
        }
        assert_eq!(Platform::ALL.len(), WirePlatform::ALL.len());
    }
}
