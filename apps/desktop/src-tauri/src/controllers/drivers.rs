//! Driver availability is probed on demand on the blocking pool, never polled.
use serde::{Deserialize, Serialize};
use specta::Type;

pub const UDEV_RULE: &str = include_str!("../../resources/70-vgames-uinput.rules");
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControllerDriverState {
    Available,
    NotRequired,
    VigembusMissing,
    UinputDenied { udev_rule: String },
    HidhideMissing,
    MacosEntitlementMissing,
    ProbeError { detail: String },
}
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ControllerDriverStatus {
    pub emulation: ControllerDriverState,
    pub hiding: ControllerDriverState,
}

#[cfg(target_os = "linux")]
pub fn probe() -> ControllerDriverStatus {
    let emulation = access_state(evdev::uinput::VirtualDevice::builder().map(drop));
    ControllerDriverStatus {
        emulation,
        hiding: ControllerDriverState::NotRequired,
    }
}
#[cfg(target_os = "linux")]
fn access_state(result: std::io::Result<()>) -> ControllerDriverState {
    match result {
        Ok(()) => ControllerDriverState::Available,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
            ) =>
        {
            ControllerDriverState::UinputDenied {
                udev_rule: UDEV_RULE.trim().into(),
            }
        }
        Err(error) => ControllerDriverState::ProbeError {
            detail: error.to_string(),
        },
    }
}
#[cfg(windows)]
pub fn probe() -> ControllerDriverStatus {
    let emulation = match vigem_client::Client::connect() {
        Ok(_) => ControllerDriverState::Available,
        Err(vigem_client::Error::BusNotFound) => ControllerDriverState::VigembusMissing,
        Err(error) => ControllerDriverState::ProbeError {
            detail: error.to_string(),
        },
    };
    let hiding = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(r"\\.\HidHide")
    {
        Ok(_) => ControllerDriverState::Available,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ControllerDriverState::HidhideMissing
        }
        Err(error) => ControllerDriverState::ProbeError {
            detail: error.to_string(),
        },
    };
    ControllerDriverStatus { emulation, hiding }
}
#[cfg(target_os = "macos")]
pub fn probe() -> ControllerDriverStatus {
    // Our unsigned distribution has no restricted entitlement or Swift bridge.
    // The virtual backend independently refuses creation; input stays native.
    ControllerDriverStatus {
        emulation: ControllerDriverState::MacosEntitlementMissing,
        hiding: ControllerDriverState::NotRequired,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn denied_access_is_actionable_and_unexpected_errors_are_not_hidden() {
        for kind in [
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::NotFound,
        ] {
            let ControllerDriverState::UinputDenied { udev_rule } = access_state(Err(kind.into()))
            else {
                panic!("access help")
            };
            assert!(udev_rule.contains("KERNEL==\"uinput\""));
        }
        assert!(matches!(
            access_state(Err(std::io::Error::other("device error"))),
            ControllerDriverState::ProbeError { .. }
        ));
        assert!(matches!(
            access_state(Ok(())),
            ControllerDriverState::Available
        ));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn unsigned_builds_never_offer_virtual_corehid() {
        assert!(matches!(
            probe().emulation,
            ControllerDriverState::MacosEntitlementMissing
        ));
    }
    #[cfg(windows)]
    #[test]
    fn native_driver_detection_returns_a_visible_state() {
        let state = probe();
        assert!(matches!(
            state.emulation,
            ControllerDriverState::Available
                | ControllerDriverState::VigembusMissing
                | ControllerDriverState::ProbeError { .. }
        ));
    }
}
