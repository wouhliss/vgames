import { describe, expect, it } from "vitest";
import { acceleratorFrom } from "./hotkey";

const key = (
  code: string,
  mods: Partial<Record<"ctrl" | "alt" | "shift" | "meta", boolean>> = {},
) => ({
  code,
  ctrlKey: Boolean(mods.ctrl),
  altKey: Boolean(mods.alt),
  shiftKey: Boolean(mods.shift),
  metaKey: Boolean(mods.meta),
});

describe("acceleratorFrom", () => {
  it("names combinations in a fixed modifier order", () => {
    expect(acceleratorFrom(key("KeyO", { shift: true, ctrl: true }))).toEqual({
      kind: "ok",
      accelerator: "Ctrl+Shift+O",
    });
    expect(acceleratorFrom(key("F3", { shift: true }))).toEqual({
      kind: "ok",
      accelerator: "Shift+F3",
    });
    expect(acceleratorFrom(key("Digit1", { alt: true, meta: true }))).toEqual({
      kind: "ok",
      accelerator: "Alt+Super+1",
    });
    expect(acceleratorFrom(key("ArrowUp", { ctrl: true }))).toEqual({
      kind: "ok",
      accelerator: "Ctrl+Up",
    });
  });

  it("allows function keys on their own", () => {
    expect(acceleratorFrom(key("F10"))).toEqual({ kind: "ok", accelerator: "F10" });
  });

  it("refuses keys that would fire while typing", () => {
    expect(acceleratorFrom(key("KeyA"))).toEqual({ kind: "needs_modifier" });
    expect(acceleratorFrom(key("KeyA", { shift: true }))).toEqual({ kind: "needs_modifier" });
    expect(acceleratorFrom(key("Space"))).toEqual({ kind: "needs_modifier" });
  });

  it("waits while only modifiers are held", () => {
    expect(acceleratorFrom(key("ShiftLeft", { shift: true }))).toEqual({ kind: "pending" });
    expect(acceleratorFrom(key("ControlRight", { ctrl: true }))).toEqual({ kind: "pending" });
  });

  it("refuses keys it can't name", () => {
    expect(acceleratorFrom(key("MediaPlayPause", { ctrl: true }))).toEqual({ kind: "unsupported" });
    expect(acceleratorFrom(key("", { ctrl: true }))).toEqual({ kind: "unsupported" });
  });
});
