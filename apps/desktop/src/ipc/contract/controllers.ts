// Controllers (07-controllers): connected pads, the live tester, the virtual-pad backend status, and
// the per-package emulation override and mapping profile for Settings → Controllers. Requested shapes;
// see core.ts for the conventions. The Rust core owns SDL, the emulation decision and mapping
// (`controllers::{decision, mapping}`); the UI only shows and edits them.
import type { ControllerKind, PackageRef } from "../../bindings";
import type { AppError } from "./core";
import { call, makeEvents, type Result } from "./runtime";

/** A connected physical pad. */
export type Pad = {
  /** SDL joystick instance id; matches `ControllerEvent.instance_id`. */
  instance_id: number;
  kind: ControllerKind;
  name: string;
  connection: "usb" | "bluetooth" | "unknown";
  /** Null when the pad can't report it (wired pads, generic pads). */
  battery: { percent: number; charging: boolean } | null;
  /** 1-based player slot, null when the pad has none. */
  player: number | null;
};

/** The virtual-pad backend of this platform (07 §4). Emulation only works when it is `ready`. */
export type BackendStatus =
  /** Everything needed is in place. */
  | { kind: "ready"; backend: "vigem" | "uinput" | "corehid" }
  /** Windows: ViGEmBus isn't installed. `help_url` is the official installer page (https). */
  | { kind: "driver_missing"; backend: "vigem"; help_url: string }
  /** Linux: no access to `/dev/uinput`; the udev rule explains how. */
  | { kind: "no_permission"; backend: "uinput"; help_url: string }
  /** macOS: this build lacks Apple's virtual HID entitlement (07 §4). */
  | { kind: "disabled_build"; backend: "corehid" }
  /** macOS: games read pads through Apple's framework, nothing to set up (07 §4.1). */
  | { kind: "not_needed" };

export type ControllersOverview = { pads: Pad[]; backend: BackendStatus };

/** Buttons of the canonical (SDL gamepad) layout, as in `controllers::mapping::Button`. */
export type PadButton =
  | "south"
  | "east"
  | "west"
  | "north"
  | "back"
  | "guide"
  | "start"
  | "left_stick"
  | "right_stick"
  | "left_shoulder"
  | "right_shoulder"
  | "dpad_up"
  | "dpad_down"
  | "dpad_left"
  | "dpad_right"
  | "touchpad";

/** XInput buttons, as in `controllers::mapping::XButton`. */
export type XInputButton =
  | "dpad_up"
  | "dpad_down"
  | "dpad_left"
  | "dpad_right"
  | "start"
  | "back"
  | "left_thumb"
  | "right_thumb"
  | "left_shoulder"
  | "right_shoulder"
  | "guide"
  | "a"
  | "b"
  | "x"
  | "y";

/** Per-stick settings in percent of full deflection (defaults: deadzone 8, anti-deadzone 0). */
export type StickProfile = { deadzone: number; anti_deadzone: number; invert_y: boolean };

/** `to: null` disables the button. */
export type ButtonRemap = { from: PadButton; to: XInputButton | null };

/** The JSON the core stores and exports (`controllers::mapping::Profile`). */
export type PadProfile = {
  /** "Use Nintendo button labels": Switch Pro face buttons map by label, not position. */
  nintendo_labels: boolean;
  left_stick: StickProfile;
  right_stick: StickProfile;
  remap: ButtonRemap[];
};

/** `auto`: the manifest and the connected pads decide (07 §3). */
export type EmulationMode = "auto" | "always" | "never";

/** An installed package and its controller settings. */
export type PackageControllers = {
  package: PackageRef;
  title: string;
  /** Proton and Wine map pads to XInput themselves, so emulation never applies. */
  compat_layer: boolean;
  /** The package's manifest declares which controllers it supports. */
  declares_support: boolean;
  emulation: EmulationMode;
  /** Null = the default profile applies. */
  profile: PadProfile | null;
};

export type ControllerSettingsError =
  | { kind: "not_found" }
  /** A deadzone or anti-deadzone is 100 % or more. */
  | { kind: "range" }
  | { kind: "duplicate_remap"; button: PadButton }
  /** Imported text isn't a valid profile (or is too large). */
  | { kind: "syntax"; detail: string }
  | { kind: "io"; detail: string };

/** The state of the pad being tested, sent only while the tester runs. */
export type PadInput = {
  instance_id: number;
  pressed: PadButton[];
  /** -32768..32767, Y up. */
  left: [number, number];
  right: [number, number];
  /** 0..32767. */
  left_trigger: number;
  right_trigger: number;
};

export const controllerCommands = {
  async controllersOverview(): Promise<Result<ControllersOverview, AppError>> {
    return call("controllers_overview");
  },
  /** Starts streaming `controller-input` for every pad; the core stops when `controllerTesterStop` is called or the window closes. */
  async controllerTesterStart(): Promise<Result<null, AppError>> {
    return call("controller_tester_start");
  },
  async controllerTesterStop(): Promise<Result<null, AppError>> {
    return call("controller_tester_stop");
  },
  async controllersPackages(): Promise<Result<PackageControllers[], AppError>> {
    return call("controllers_packages");
  },
  async controllerEmulationSet(
    pkg: PackageRef,
    mode: EmulationMode,
  ): Promise<Result<null, ControllerSettingsError>> {
    return call("controller_emulation_set", { package: pkg, mode });
  },
  /** The default profile (used by every package without its own). */
  async controllerDefaultProfile(): Promise<Result<PadProfile, AppError>> {
    return call("controller_default_profile");
  },
  /** `pkg: null` saves the default profile. */
  async controllerProfileSet(
    pkg: PackageRef | null,
    profile: PadProfile,
  ): Promise<Result<null, ControllerSettingsError>> {
    return call("controller_profile_set", { package: pkg, profile });
  },
  /** Back to the default profile (a package) or to the built-in one (`pkg: null`). */
  async controllerProfileReset(pkg: PackageRef | null): Promise<Result<null, AppError>> {
    return call("controller_profile_reset", { package: pkg });
  },
  /** The profile as JSON text, to copy. */
  async controllerProfileExport(pkg: PackageRef | null): Promise<Result<string, AppError>> {
    return call("controller_profile_export", { package: pkg });
  },
  /** Parses and validates pasted JSON; nothing is saved until `controllerProfileSet`. */
  async controllerProfileParse(json: string): Promise<Result<PadProfile, ControllerSettingsError>> {
    return call("controller_profile_parse", { json });
  },
};

export const controllerEvents = makeEvents<{
  controllerInput: PadInput;
  controllersChanged: Record<string, never>;
}>({
  controllerInput: "controller-input",
  controllersChanged: "controllers-changed",
});
