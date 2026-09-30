// Controllers: pads, the virtual-pad backend, per-package emulation and mapping profiles
// (07-controllers). Validation mirrors `controllers::mapping::Profile::validate`. The live tester
// streams nothing by itself: a test (or the dev page) emits `controller-input`.
import type {
  BackendStatus,
  ControllerSettingsError,
  EmulationMode,
  InstalledPackage,
  PackageControllers,
  PackageRef,
  Pad,
  PadButton,
  PadProfile,
} from "../ipc";
import { fail, type Handler } from "./runtime";

export const BUILT_IN_PROFILE: PadProfile = {
  nintendo_labels: false,
  left_stick: { deadzone: 8, anti_deadzone: 0, invert_y: false },
  right_stick: { deadzone: 8, anti_deadzone: 0, invert_y: false },
  remap: [],
};

export interface ControllersState {
  controllers: {
    pads: Pad[];
    backend: BackendStatus;
    /** By package id. */
    emulation: Record<string, EmulationMode>;
    /** By package id; `default` is the profile every package without its own uses. */
    profiles: Record<string, PadProfile>;
    testerRunning: boolean;
    /** Forced results by command. */
    errors: Record<string, ControllerSettingsError>;
  };
}

export function defaultControllersState(): ControllersState {
  return {
    controllers: {
      pads: [
        {
          instance_id: 1,
          kind: "dualsense",
          name: "DualSense Wireless Controller",
          connection: "bluetooth",
          battery: { percent: 64, charging: false },
          player: 1,
        },
        {
          instance_id: 2,
          kind: "xinput",
          name: "Xbox Wireless Controller",
          connection: "usb",
          battery: null,
          player: 2,
        },
      ],
      backend: { kind: "ready", backend: "uinput" },
      emulation: {},
      profiles: {},
      testerRunning: false,
      errors: {},
    },
  };
}

const STICK_MAX = 99;

/** The same rules as the Rust core, so the UI's own checks and the core's agree. */
export function validateProfile(profile: PadProfile): ControllerSettingsError | null {
  for (const stick of [profile.left_stick, profile.right_stick]) {
    if (stick.deadzone > STICK_MAX || stick.anti_deadzone > STICK_MAX) return { kind: "range" };
  }
  const seen = new Set<PadButton>();
  for (const remap of profile.remap) {
    if (seen.has(remap.from)) return { kind: "duplicate_remap", button: remap.from };
    seen.add(remap.from);
  }
  return null;
}

export function controllersHandlers(
  state: ControllersState & { installs: InstalledPackage[] },
): Record<string, Handler> {
  const c = state.controllers;
  const forced = (cmd: string) => {
    const error = c.errors[cmd];
    if (error) fail(error);
  };
  const key = (ref: PackageRef | null) => (ref ? ref.package_id : "default");
  const installOf = (ref: PackageRef) =>
    state.installs.find((i) => i.package.package_id === ref.package_id);

  return {
    controllers_overview: () => ({ pads: c.pads, backend: c.backend }),
    controller_tester_start: () => {
      c.testerRunning = true;
      return null;
    },
    controller_tester_stop: () => {
      c.testerRunning = false;
      return null;
    },
    controllers_packages: (): PackageControllers[] =>
      state.installs
        .filter((i) => i.state === "installed" || i.state === "incomplete")
        .map((i) => ({
          package: i.package,
          title: i.title,
          compat_layer: i.compat !== "native",
          declares_support: i.title.length % 2 === 0,
          emulation: c.emulation[i.package.package_id] ?? "auto",
          profile: c.profiles[i.package.package_id] ?? null,
        })),
    controller_emulation_set: (args) => {
      forced("controller_emulation_set");
      const ref = args.package as PackageRef;
      if (!installOf(ref)) fail({ kind: "not_found" } satisfies ControllerSettingsError);
      c.emulation[ref.package_id] = args.mode as EmulationMode;
      return null;
    },
    controller_default_profile: () => c.profiles.default ?? BUILT_IN_PROFILE,
    controller_profile_set: (args) => {
      forced("controller_profile_set");
      const ref = args.package as PackageRef | null;
      if (ref && !installOf(ref)) fail({ kind: "not_found" } satisfies ControllerSettingsError);
      const profile = args.profile as PadProfile;
      const invalid = validateProfile(profile);
      if (invalid) fail(invalid);
      c.profiles[key(ref)] = profile;
      return null;
    },
    controller_profile_reset: (args) => {
      delete c.profiles[key(args.package as PackageRef | null)];
      return null;
    },
    controller_profile_export: (args) => {
      const ref = args.package as PackageRef | null;
      const profile = c.profiles[key(ref)] ?? c.profiles.default ?? BUILT_IN_PROFILE;
      return JSON.stringify(profile, null, 2);
    },
    controller_profile_parse: (args) => {
      forced("controller_profile_parse");
      const json = String(args.json);
      if (json.length > 16 * 1024)
        fail({ kind: "syntax", detail: "too large" } satisfies ControllerSettingsError);
      let parsed: PadProfile;
      try {
        parsed = JSON.parse(json) as PadProfile;
      } catch (e) {
        fail({ kind: "syntax", detail: String(e) } satisfies ControllerSettingsError);
      }
      if (typeof parsed !== "object" || parsed === null || !Array.isArray(parsed.remap))
        fail({ kind: "syntax", detail: "missing fields" } satisfies ControllerSettingsError);
      const invalid = validateProfile(parsed);
      if (invalid) fail(invalid);
      return parsed;
    },
  };
}
