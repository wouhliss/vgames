// PLAY-04 pending commands/events; replace with generated bindings as the Rust lands.
// Shapes follow 07-controllers-notes and the existing Rust mapping/decision modules.
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
export type {
  ControllerDriverState,
  ControllerDriverStatus,
  ControllerTesterState,
  Tester as ControllerTester,
} from "../../bindings";

export const controllerCommands = {};
