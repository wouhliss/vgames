//! Checks before a Proton launch (09-compatibility §2, GAME-05). Each failure has
//! a specific message and a fix hint; probes are a trait so every failure is tested.

use std::path::Path;

/// Free space a new prefix needs (09 §2: about 1 GB).
pub const PREFIX_SPACE_BYTES: u64 = 1024 * 1024 * 1024;

/// Ubuntu 24.04's AppArmor switch that blocks pressure-vessel's user namespaces.
pub const APPARMOR_USERNS_SYSCTL: &str = "kernel.apparmor_restrict_unprivileged_userns";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PreflightError {
    #[error(
        "No Vulkan driver was found. Proton needs one: install your graphics vendor's Vulkan driver (for example mesa-vulkan-drivers)."
    )]
    NoVulkan,
    #[error(
        "Unprivileged user namespaces are blocked by AppArmor ({sysctl} = 1), and Proton's container needs them. Set it to 0 or add an AppArmor profile that allows them."
    )]
    UserNamespacesRestricted { sysctl: &'static str },
    #[error(
        "Unprivileged user namespaces are turned off ({sysctl} = 0), and Proton's container needs them. Turn them on to play this game."
    )]
    UserNamespacesDisabled { sysctl: &'static str },
    #[error(
        "Not enough free space for this game's Windows prefix: {needed_mb} MB needed, {free_mb} MB free."
    )]
    LowDiskSpace { needed_mb: u64, free_mb: u64 },
}

/// What the checks read from the system.
pub trait Probes {
    /// Physical devices a Vulkan instance reports (`None`: no loader or driver).
    fn vulkan_devices(&self) -> Option<usize>;
    /// A `/proc/sys` value by sysctl name, trimmed; `None` when absent.
    fn sysctl(&self, name: &str) -> Option<String>;
    /// Free bytes on the file system holding `path`.
    fn free_space(&self, path: &Path) -> Option<u64>;
    /// Whether x86_64 code runs on this Mac (Rosetta 2 on Apple silicon). Blocking.
    fn x86_64_runs(&self) -> bool {
        false
    }
}

/// Runs every check; `prefix_exists` skips the space check for an existing prefix.
pub fn check_proton(
    probes: &impl Probes,
    prefix: &Path,
    prefix_exists: bool,
) -> Result<(), PreflightError> {
    if probes.vulkan_devices().unwrap_or(0) == 0 {
        return Err(PreflightError::NoVulkan);
    }
    if probes.sysctl(APPARMOR_USERNS_SYSCTL).as_deref() == Some("1") {
        return Err(PreflightError::UserNamespacesRestricted {
            sysctl: APPARMOR_USERNS_SYSCTL,
        });
    }
    for sysctl in [
        "kernel.unprivileged_userns_clone",
        "user.max_user_namespaces",
    ] {
        if probes.sysctl(sysctl).as_deref() == Some("0") {
            return Err(PreflightError::UserNamespacesDisabled { sysctl });
        }
    }
    if !prefix_exists {
        // The closest existing ancestor holds the new prefix.
        let probe_at = prefix.ancestors().find(|p| p.exists()).unwrap_or(prefix);
        if let Some(free) = probes.free_space(probe_at)
            && free < PREFIX_SPACE_BYTES
        {
            return Err(PreflightError::LowDiskSpace {
                needed_mb: PREFIX_SPACE_BYTES / 1_000_000,
                free_mb: free / 1_000_000,
            });
        }
    }
    Ok(())
}

/// The real system.
pub struct System;

impl Probes for System {
    fn vulkan_devices(&self) -> Option<usize> {
        vulkan_device_count()
    }

    fn sysctl(&self, name: &str) -> Option<String> {
        let path = Path::new("/proc/sys").join(name.replace('.', "/"));
        std::fs::read_to_string(path)
            .ok()
            .map(|v| v.trim().to_owned())
    }

    fn free_space(&self, path: &Path) -> Option<u64> {
        vgames_transfer::sys::available_space(path).ok()
    }

    fn x86_64_runs(&self) -> bool {
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            std::process::Command::new("arch")
                .args(["-x86_64", "/usr/bin/true"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        } else {
            cfg!(target_os = "macos")
        }
    }
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn vulkan_device_count() -> Option<usize> {
    // SAFETY: `Entry::load` dlopens the system Vulkan loader; the instance is created
    // with a valid, fully initialized create info, used only on this thread, and
    // destroyed before returning. No pointer outlives this function.
    unsafe {
        let entry = ash::Entry::load().ok()?;
        let app = ash::vk::ApplicationInfo::default().api_version(ash::vk::API_VERSION_1_1);
        let info = ash::vk::InstanceCreateInfo::default().application_info(&app);
        let instance = entry.create_instance(&info, None).ok()?;
        let count = instance.enumerate_physical_devices().map(|d| d.len()).ok();
        instance.destroy_instance(None);
        count
    }
}

#[cfg(not(target_os = "linux"))]
fn vulkan_device_count() -> Option<usize> {
    None
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct Fake {
        vulkan: Option<usize>,
        sysctl: HashMap<&'static str, &'static str>,
        free: Option<u64>,
    }

    impl Probes for Fake {
        fn vulkan_devices(&self) -> Option<usize> {
            self.vulkan
        }
        fn sysctl(&self, name: &str) -> Option<String> {
            self.sysctl.get(name).map(|v| (*v).to_owned())
        }
        fn free_space(&self, _: &Path) -> Option<u64> {
            self.free
        }
    }

    fn healthy() -> Fake {
        Fake {
            vulkan: Some(1),
            sysctl: HashMap::from([(APPARMOR_USERNS_SYSCTL, "0")]),
            free: Some(50 * PREFIX_SPACE_BYTES),
        }
    }

    #[test]
    fn a_healthy_system_passes() {
        assert_eq!(
            check_proton(&healthy(), Path::new("/x/prefix"), false),
            Ok(())
        );
    }

    #[test]
    fn each_failure_has_its_own_message() {
        let p = Path::new("/x/prefix");
        let mut f = healthy();
        f.vulkan = None;
        assert_eq!(check_proton(&f, p, true), Err(PreflightError::NoVulkan));
        f.vulkan = Some(0);
        assert_eq!(check_proton(&f, p, true), Err(PreflightError::NoVulkan));

        let mut f = healthy();
        f.sysctl.insert(APPARMOR_USERNS_SYSCTL, "1");
        let err = check_proton(&f, p, true).unwrap_err();
        assert!(err.to_string().contains(APPARMOR_USERNS_SYSCTL), "{err}");

        let mut f = healthy();
        f.sysctl.insert("kernel.unprivileged_userns_clone", "0");
        assert_eq!(
            check_proton(&f, p, true),
            Err(PreflightError::UserNamespacesDisabled {
                sysctl: "kernel.unprivileged_userns_clone"
            })
        );

        let mut f = healthy();
        f.free = Some(PREFIX_SPACE_BYTES / 2);
        let err = check_proton(&f, p, false).unwrap_err();
        assert!(
            matches!(
                err,
                PreflightError::LowDiskSpace {
                    needed_mb: 1073,
                    free_mb: 536
                }
            ),
            "{err:?}"
        );
        // An existing prefix needs no new space.
        assert_eq!(check_proton(&f, p, true), Ok(()));
    }

    /// `VGAMES_EXPECT_VULKAN=1`: the host has a Vulkan driver (compat-e2e.yml, lavapipe).
    #[test]
    fn the_real_probes_read_without_panicking() {
        if std::env::var_os("VGAMES_EXPECT_VULKAN").is_some() {
            assert!(
                System.vulkan_devices().unwrap_or(0) > 0,
                "no Vulkan device found"
            );
        }
        let _ = System.vulkan_devices();
        let _ = System.sysctl(APPARMOR_USERNS_SYSCTL);
        let _ = System.free_space(Path::new("/"));
    }
}
