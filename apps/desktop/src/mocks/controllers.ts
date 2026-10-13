// Controller fixtures for PLAY-04 Settings, including unavailable driver states.
import type {
  AppError,
  AppInfo,
  ConnectedPad,
  ControllerDriverStatus,
  ControllerList,
  ControllerProfile,
  ControllerSettings,
  ControllerTester,
  PackageRef,
} from "../ipc";
import { fail, type Handler } from "./runtime";

export interface ControllersState {
  controllers: ControllerList;
  controllerSettings: ControllerSettings;
  controllerProfiles: Record<string, ControllerProfile>;
  /** null selects the normal missing-driver state for the mock host. */
  controllerDrivers: ControllerDriverStatus | null;
}

export const MOCK_DUALSENSE: ConnectedPad = {
  instance_id: 1,
  kind: "dualsense",
  name: "DualSense",
  connection: "usb",
  battery: { percent: null, state: "wired" },
  player_index: 0,
};

export function defaultControllerProfile(nintendoLabels = false): ControllerProfile {
  return {
    mode: "auto",
    mapping: {
      nintendo_labels: nintendoLabels,
      left_stick: { deadzone: 8, anti_deadzone: 0, invert_y: false },
      right_stick: { deadzone: 8, anti_deadzone: 0, invert_y: false },
      remap: [],
    },
  };
}

export function defaultControllersState(): ControllersState {
  return {
    controllers: { state: { kind: "ready" }, pads: [] },
    controllerSettings: { nintendo_labels: false },
    controllerProfiles: {},
    controllerDrivers: null,
  };
}

const BUTTONS = new Set([
  "south",
  "east",
  "west",
  "north",
  "back",
  "guide",
  "start",
  "left_stick",
  "right_stick",
  "left_shoulder",
  "right_shoulder",
  "dpad_up",
  "dpad_down",
  "dpad_left",
  "dpad_right",
  "touchpad",
]);
const OUTPUTS = new Set([
  "a",
  "b",
  "x",
  "y",
  "back",
  "guide",
  "start",
  "left_thumb",
  "right_thumb",
  "left_shoulder",
  "right_shoulder",
  "dpad_up",
  "dpad_down",
  "dpad_left",
  "dpad_right",
]);
function invalid(field: string): never {
  return fail({
    kind: "invalid_input",
    field,
    message: "Invalid controller setting.",
  } satisfies AppError);
}

function validate(profile: ControllerProfile): void {
  if (!profile || !["auto", "always", "never"].includes(profile.mode) || !profile.mapping)
    invalid("profile");
  const mapping = profile.mapping;
  if (typeof mapping.nintendo_labels !== "boolean") invalid("profile");
  for (const stick of [mapping.left_stick, mapping.right_stick]) {
    if (!stick || typeof stick.invert_y !== "boolean") invalid("profile");
    for (const value of [stick.deadzone, stick.anti_deadzone]) {
      if (!Number.isInteger(value) || value < 0 || value >= 100) invalid("deadzone");
    }
  }
  if (!Array.isArray(mapping.remap) || mapping.remap.length > 16) invalid("remap");
  const seen = new Set<string>();
  for (const remap of mapping.remap) {
    if (
      !remap ||
      !BUTTONS.has(remap.from) ||
      seen.has(remap.from) ||
      (remap.to !== null && !OUTPUTS.has(remap.to))
    )
      invalid("remap");
    seen.add(remap.from);
  }
}

function profileKey(pkg: PackageRef): string {
  if (!pkg?.server_id || !pkg.package_id) invalid("package_ref");
  return `${pkg.server_id}/${pkg.package_id}`;
}

const UDEV_RULE = 'KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess"';

export function controllerHandlers(
  state: ControllersState & { appInfo: AppInfo },
): Record<string, Handler> {
  const subscriptions = new Map<string, ControllerTester>();
  let sequence = 0;
  return {
    controllers_list: () => structuredClone(state.controllers),
    controllers_settings_get: () => ({ ...state.controllerSettings }),
    controllers_settings_set: (args) => {
      const settings = args.settings as ControllerSettings;
      if (!settings || typeof settings.nintendo_labels !== "boolean") invalid("settings");
      state.controllerSettings = { ...settings };
      return { ...settings };
    },
    controllers_profile_get: (args) => {
      const profile =
        state.controllerProfiles[profileKey(args.packageRef as PackageRef)] ??
        defaultControllerProfile(state.controllerSettings.nintendo_labels);
      return structuredClone(profile);
    },
    controllers_profile_set: (args) => {
      const key = profileKey(args.packageRef as PackageRef);
      const profile = args.profile as ControllerProfile;
      validate(profile);
      state.controllerProfiles = { ...state.controllerProfiles, [key]: structuredClone(profile) };
      return structuredClone(profile);
    },
    controllers_driver_status: () => {
      if (state.controllerDrivers) return structuredClone(state.controllerDrivers);
      switch (state.appInfo.os) {
        case "windows":
          return {
            emulation: { kind: "vigembus_missing" },
            hiding: { kind: "hidhide_missing" },
          } satisfies ControllerDriverStatus;
        case "macos":
          return {
            emulation: { kind: "macos_entitlement_missing" },
            hiding: { kind: "not_required" },
          } satisfies ControllerDriverStatus;
        default:
          return {
            emulation: { kind: "uinput_denied", udev_rule: UDEV_RULE },
            hiding: { kind: "not_required" },
          } satisfies ControllerDriverStatus;
      }
    },
    controllers_tester_start: (args) => {
      if (
        state.controllers.state.kind !== "ready" ||
        !state.controllers.pads.some((pad) => pad.instance_id === args.instanceId)
      )
        fail({ kind: "not_found" } satisfies AppError);
      const tester: ControllerTester = {
        subscription_id: `controller-tester-${++sequence}`,
        instance_id: Number(args.instanceId),
      };
      subscriptions.set(tester.subscription_id, tester);
      return { ...tester };
    },
    controllers_tester_stop: (args) => {
      subscriptions.delete(String(args.subscriptionId));
      return null;
    },
  };
}
