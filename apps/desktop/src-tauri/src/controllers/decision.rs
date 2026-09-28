//! Per-launch emulation decision (07-controllers §3).

use serde::{Deserialize, Serialize};
use vgames_core::manifest::{self, ControllerKind as ManifestKind, EmulatedController};

use crate::events::ControllerKind;

/// The player's per-package choice (`controller_profiles.mode`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmulationMode {
    #[default]
    Auto,
    Always,
    Never,
}

/// What to do with one connected pad for this launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadDecision {
    /// The game sees the physical pad.
    Passthrough,
    /// Create a virtual pad of this kind and hide the physical one.
    Emulate(EmulatedController),
    /// The game probably cannot use this pad and no emulation is possible:
    /// pass it through and show a one-time notice.
    PassthroughWithNotice,
}

/// Inputs that apply to every pad of a launch.
#[derive(Debug, Clone, Copy)]
pub struct LaunchControllers<'a> {
    /// The manifest's `controllers` (absent: the package declares nothing).
    pub manifest: Option<&'a manifest::Controllers>,
    /// Proton or Wine map pads to XInput themselves (09-compatibility).
    pub compat_layer: bool,
    pub mode: EmulationMode,
    /// A virtual-pad backend is installed and usable (ViGEmBus, uinput, CoreHID).
    pub backend_available: bool,
}

pub fn decide(launch: &LaunchControllers<'_>, pad: ControllerKind) -> PadDecision {
    if launch.compat_layer || launch.mode == EmulationMode::Never {
        return PadDecision::Passthrough;
    }
    let supported = launch
        .manifest
        .map(|c| c.supported.as_slice())
        .unwrap_or_default();
    let target = launch.manifest.and_then(|c| c.emulate_as);
    let pad_supported = supported.is_empty() || supported.contains(&manifest_kind(pad));
    let wants_emulation = !pad_supported || launch.mode == EmulationMode::Always;
    match target {
        // "Always" emulates even a supported pad, but never a pad into its own kind.
        Some(target) if wants_emulation && !same_kind(pad, target) => {
            if launch.backend_available {
                PadDecision::Emulate(target)
            } else if pad_supported {
                PadDecision::Passthrough
            } else {
                PadDecision::PassthroughWithNotice
            }
        }
        _ if pad_supported => PadDecision::Passthrough,
        _ => PadDecision::PassthroughWithNotice,
    }
}

fn manifest_kind(kind: ControllerKind) -> ManifestKind {
    match kind {
        ControllerKind::Xinput => ManifestKind::Xinput,
        ControllerKind::Dualshock4 => ManifestKind::Dualshock4,
        ControllerKind::Dualsense => ManifestKind::Dualsense,
        ControllerKind::SwitchPro => ManifestKind::SwitchPro,
        ControllerKind::Generic => ManifestKind::Generic,
    }
}

fn same_kind(pad: ControllerKind, target: EmulatedController) -> bool {
    matches!(
        (pad, target),
        (ControllerKind::Xinput, EmulatedController::Xbox360)
            | (ControllerKind::Dualshock4, EmulatedController::Dualshock4)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controllers(
        supported: &[ManifestKind],
        emulate_as: Option<EmulatedController>,
    ) -> manifest::Controllers {
        manifest::Controllers {
            supported: supported.to_vec(),
            emulate_as,
        }
    }

    fn launch(c: Option<&manifest::Controllers>) -> LaunchControllers<'_> {
        LaunchControllers {
            manifest: c,
            compat_layer: false,
            mode: EmulationMode::Auto,
            backend_available: true,
        }
    }

    #[test]
    fn unsupported_pads_are_emulated_when_possible() {
        let xbox_only = controllers(&[ManifestKind::Xinput], Some(EmulatedController::Xbox360));
        let l = launch(Some(&xbox_only));
        assert_eq!(
            decide(&l, ControllerKind::Dualsense),
            PadDecision::Emulate(EmulatedController::Xbox360)
        );
        assert_eq!(
            decide(&l, ControllerKind::SwitchPro),
            PadDecision::Emulate(EmulatedController::Xbox360)
        );
        assert_eq!(decide(&l, ControllerKind::Xinput), PadDecision::Passthrough);

        let no_backend = LaunchControllers {
            backend_available: false,
            ..l
        };
        assert_eq!(
            decide(&no_backend, ControllerKind::Dualsense),
            PadDecision::PassthroughWithNotice
        );

        let no_target = controllers(&[ManifestKind::Xinput], None);
        assert_eq!(
            decide(&launch(Some(&no_target)), ControllerKind::Dualsense),
            PadDecision::PassthroughWithNotice
        );
    }

    #[test]
    fn compat_layers_never_and_empty_support_pass_through() {
        let xbox_only = controllers(&[ManifestKind::Xinput], Some(EmulatedController::Xbox360));
        let proton = LaunchControllers {
            compat_layer: true,
            ..launch(Some(&xbox_only))
        };
        assert_eq!(
            decide(&proton, ControllerKind::Dualsense),
            PadDecision::Passthrough
        );
        let never = LaunchControllers {
            mode: EmulationMode::Never,
            ..launch(Some(&xbox_only))
        };
        assert_eq!(
            decide(&never, ControllerKind::Dualsense),
            PadDecision::Passthrough
        );
        let anything = controllers(&[], Some(EmulatedController::Xbox360));
        assert_eq!(
            decide(&launch(Some(&anything)), ControllerKind::Generic),
            PadDecision::Passthrough
        );
        assert_eq!(
            decide(&launch(None), ControllerKind::SwitchPro),
            PadDecision::Passthrough
        );
    }

    #[test]
    fn always_emulates_supported_pads_but_not_into_their_own_kind() {
        let both = controllers(
            &[ManifestKind::Xinput, ManifestKind::Dualsense],
            Some(EmulatedController::Xbox360),
        );
        let always = LaunchControllers {
            mode: EmulationMode::Always,
            ..launch(Some(&both))
        };
        assert_eq!(
            decide(&always, ControllerKind::Dualsense),
            PadDecision::Emulate(EmulatedController::Xbox360)
        );
        assert_eq!(
            decide(&always, ControllerKind::Xinput),
            PadDecision::Passthrough
        );
        let no_backend = LaunchControllers {
            backend_available: false,
            ..always
        };
        assert_eq!(
            decide(&no_backend, ControllerKind::Dualsense),
            PadDecision::Passthrough
        );
    }
}
