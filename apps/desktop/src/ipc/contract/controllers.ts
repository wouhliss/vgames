// PLAY-04 pending commands/events; replace with generated bindings as the Rust lands.
// Shapes follow 07-controllers-notes and the existing Rust mapping/decision modules.
import type { AppError } from "../../bindings";
import { call, type Result } from "./runtime";

export type ControllerConnection = "usb" | "bluetooth" | "unknown";
export type ControllerBattery = {
  percent: number | null;
  state: "charging" | "on_battery" | "wired" | "unknown";
};
export type {
  ConnectedPad,
  ControllerList,
  ControllerMapping,
  ControllerProfile,
  ControllerSettings,
  ControllerStickSettings,
} from "../../bindings";
export type ControllerButton =
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
export type XboxButton =
  | "a"
  | "b"
  | "x"
  | "y"
  | "back"
  | "guide"
  | "start"
  | "left_thumb"
  | "right_thumb"
  | "left_shoulder"
  | "right_shoulder"
  | "dpad_up"
  | "dpad_down"
  | "dpad_left"
  | "dpad_right";
export type ControllerDriverState =
  | { kind: "available" }
  | { kind: "not_required" }
  | { kind: "vigembus_missing" }
  | { kind: "uinput_denied"; udev_rule: string }
  | { kind: "hidhide_missing" }
  | { kind: "macos_entitlement_missing" }
  | { kind: "probe_error"; detail: string };
export type ControllerDriverStatus = {
  emulation: ControllerDriverState;
  hiding: ControllerDriverState;
};
export type { ControllerTesterState, Tester as ControllerTester } from "../../bindings";

export const controllerCommands = {
  controllersDriverStatus(): Promise<Result<ControllerDriverStatus, AppError>> {
    return call("controllers_driver_status");
  },
};
