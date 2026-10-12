// PLAY-04 pending commands/events; replace with generated bindings as the Rust lands.
// Shapes follow 07-controllers-notes and the existing Rust mapping/decision modules.
import type { AppError, ControllerKind } from "../../bindings";
import { call, makeEvents, type Result } from "./runtime";

export type ControllerConnection = "usb" | "bluetooth" | "unknown";
export type ControllerBattery = {
  percent: number | null;
  state: "charging" | "on_battery" | "wired" | "unknown";
};
export type ConnectedPad = {
  instance_id: number;
  kind: ControllerKind;
  name: string;
  connection: ControllerConnection;
  battery: ControllerBattery;
  player_index: number | null;
};
export type ControllerList = {
  state: { kind: "ready" } | { kind: "unavailable"; detail: string };
  pads: ConnectedPad[];
};
export type {
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
export type ControllerTester = { subscription_id: string; instance_id: number };
export type ControllerTesterState = ControllerTester & {
  buttons: ControllerButton[];
  left: [number, number];
  right: [number, number];
  left_trigger: number;
  right_trigger: number;
};

export const controllerCommands = {
  controllersList(): Promise<Result<ControllerList, AppError>> {
    return call("controllers_list");
  },
  controllersDriverStatus(): Promise<Result<ControllerDriverStatus, AppError>> {
    return call("controllers_driver_status");
  },
  controllersTesterStart(instanceId: number): Promise<Result<ControllerTester, AppError>> {
    return call("controllers_tester_start", { instanceId });
  },
  controllersTesterStop(subscriptionId: string): Promise<Result<null, AppError>> {
    return call("controllers_tester_stop", { subscriptionId });
  },
};

export const controllerEvents = makeEvents<{
  padsChanged: Record<string, never>;
  controllerTesterState: ControllerTesterState;
}>({ padsChanged: "pads-changed", controllerTesterState: "controller-tester-state" });
