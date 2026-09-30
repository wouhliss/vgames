// Pure helpers for Settings → Controllers: the remap list and the stick numbers of a profile.

import { t } from "../../i18n";
import type {
  ButtonRemap,
  ControllerSettingsError,
  PadButton,
  PadProfile,
  StickProfile,
  XInputButton,
} from "../../ipc";

export const PAD_BUTTONS: readonly PadButton[] = [
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
];

export const X_BUTTONS: readonly XInputButton[] = [
  "a",
  "b",
  "x",
  "y",
  "start",
  "back",
  "guide",
  "left_thumb",
  "right_thumb",
  "left_shoulder",
  "right_shoulder",
  "dpad_up",
  "dpad_down",
  "dpad_left",
  "dpad_right",
];

export const padButtonLabel = (b: PadButton) => t(`controllers.button.${b}`);

/** Buttons that don't have a remap yet, in layout order. */
export function unusedButtons(remap: readonly ButtonRemap[]): PadButton[] {
  const used = new Set(remap.map((r) => r.from));
  return PAD_BUTTONS.filter((b) => !used.has(b));
}

/** A whole number 0–99, or null. */
export function parsePercent(text: string): number | null {
  if (!/^\d{1,2}$/.test(text.trim())) return null;
  return Number(text.trim());
}

export function sameProfile(a: PadProfile, b: PadProfile): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

export const stickFields = (s: StickProfile) => ({
  deadzone: String(s.deadzone),
  anti: String(s.anti_deadzone),
});

export function profileErrorText(error: ControllerSettingsError | null): string {
  if (error === null) return t("controllers.profile.errors.io", { detail: "" });
  switch (error.kind) {
    case "range":
      return t("controllers.profile.errors.range");
    case "duplicate_remap":
      return t("controllers.profile.errors.duplicate", { button: padButtonLabel(error.button) });
    case "syntax":
      return t("controllers.profile.errors.syntax", { detail: error.detail });
    case "not_found":
      return t("controllers.profile.errors.notFound");
    case "io":
      return t("controllers.profile.errors.io", { detail: error.detail });
  }
}
