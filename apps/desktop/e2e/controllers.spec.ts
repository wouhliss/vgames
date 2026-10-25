import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import type { ConnectedPad } from "../src/ipc";
import { commandCalls, emit, open } from "./helpers";

const pad: ConnectedPad = {
  instance_id: 42,
  kind: "dualsense",
  name: "DualSense",
  connection: "usb",
  battery: { percent: null, state: "wired" },
  player_index: 0,
};
const ready = { controllers: { state: { kind: "ready" }, pads: [pad] } };

async function settings(page: import("@playwright/test").Page, state = {}) {
  await open(page, "ready", { ...ready, persistKey: "controller-browser-persistence", ...state });
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Settings" })
    .click();
  await page
    .getByRole("navigation", { name: "Settings sections" })
    .getByRole("link", { name: "Controllers" })
    .click();
  await expect(page.getByRole("button", { name: "Test DualSense" })).toBeVisible();
  await expect(page.getByRole("status").filter({ hasText: "Loading" })).toHaveCount(0);
}

test("controller settings and every driver state are axe clean", async ({ page }) => {
  for (const state of [
    { kind: "available" },
    { kind: "not_required" },
    { kind: "vigembus_missing" },
    { kind: "hidhide_missing" },
    { kind: "uinput_denied", udev_rule: 'KERNEL=="uinput", TAG+="uaccess"' },
    { kind: "macos_entitlement_missing" },
    { kind: "probe_error", detail: "Access denied." },
  ]) {
    await settings(page, {
      controllerDrivers: { emulation: state, hiding: { kind: "not_required" } },
    });
    expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  }
});

test("keyboard changes survive restart and tester closes cleanly", async ({ page }) => {
  await settings(page);
  const labels = page.getByRole("switch", { name: "Use Nintendo button labels" });
  await labels.focus();
  await page.keyboard.press("Space");
  await expect(labels).toHaveAttribute("aria-checked", "true");
  // Restart with the same mock storage key and rediscover the physical pad.
  await settings(page);
  await expect(labels).toHaveAttribute("aria-checked", "true");
  await page.getByRole("button", { name: "Test DualSense" }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect
    .poll(async () => (await commandCalls(page, "controllers_tester_start")).length)
    .toBe(1);
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect
    .poll(async () => (await commandCalls(page, "controllers_tester_stop")).length)
    .toBe(1);
});

test("gamepad toggles labels and drives the tester without a mouse", async ({ page }) => {
  await settings(page);
  const nav = async (action: string) =>
    emit(page, "ui-nav", { action, controller: "dualsense", repeat: false });
  const labels = page.getByRole("switch", { name: "Use Nintendo button labels" });
  await labels.focus();
  await nav("accept");
  await expect(labels).toHaveAttribute("aria-checked", "true");
  await nav("down");
  await expect(page.getByRole("button", { name: "Test DualSense" })).toBeFocused();
  await nav("accept");
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect
    .poll(async () => (await commandCalls(page, "controllers_tester_start")).length)
    .toBe(1);
  await emit(page, "controller-tester-state", {
    subscription_id: "controller-tester-1",
    instance_id: 42,
    buttons: ["south"],
    left: [0, 0],
    right: [0, 0],
    left_trigger: 32767,
    right_trigger: 0,
  });
  await expect(page.getByRole("dialog").getByText("Bottom face button")).toBeVisible();
  await nav("back");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Test DualSense" })).toBeFocused();
});

test("a per-game profile is edited and saved with the keyboard only and survives restart", async ({
  page,
}) => {
  await settings(page, { persistKey: "controller-profile-browser" });
  const game = page.getByRole("combobox", { name: "Game" });
  await game.focus();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  const save = page.getByRole("button", { name: "Save controller profile" });
  await expect(save).toBeVisible();
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);

  await page.getByRole("radio", { name: "Automatic" }).focus();
  await page.keyboard.press("ArrowRight");
  await expect(page.getByRole("radio", { name: "Always" })).toBeChecked();
  const deadzone = page.getByRole("slider", { name: "Left stick deadzone" });
  const before = Number(await deadzone.inputValue());
  await deadzone.focus();
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("ArrowRight");
  await save.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByText("Saved")).toBeVisible();
  await expect(save).toBeFocused();
  const calls = (await commandCalls(page, "controllers_profile_set")) as {
    profile: { mode: string; mapping: { left_stick: { deadzone: number } } };
  }[];
  expect(calls).toHaveLength(1);
  expect(calls[0]?.profile.mode).toBe("always");
  expect(calls[0]?.profile.mapping.left_stick.deadzone).toBe(before + 2);

  await page.reload();
  await settings(page, { persistKey: "controller-profile-browser" });
  await page.getByRole("combobox", { name: "Game" }).focus();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(page.getByRole("radio", { name: "Always" })).toBeChecked();
  await expect(page.getByRole("slider", { name: "Left stick deadzone" })).toHaveValue(
    String(before + 2),
  );
});

test("a per-game profile is edited and saved with a controller only", async ({ page }) => {
  await settings(page, { persistKey: "controller-profile-gamepad" });
  const nav = async (action: string) =>
    emit(page, "ui-nav", { action, controller: "dualsense", repeat: false });
  const focused = () =>
    page.evaluate(
      () =>
        document.activeElement?.getAttribute("aria-label") ??
        document.activeElement?.textContent ??
        "",
    );
  const game = page.getByRole("combobox", { name: "Game" });
  await game.focus();
  await nav("accept");
  await nav("down");
  await nav("accept");
  const save = page.getByRole("button", { name: "Save controller profile" });
  await expect(save).toBeVisible();
  const invert = page.getByRole("switch", { name: "Invert left stick Y" });
  // Move down until the switch has focus (bounded), then flip it and go on to Save.
  for (let i = 0; i < 12 && !(await invert.evaluate((el) => el === document.activeElement)); i++)
    await nav("down");
  await expect(invert).toBeFocused();
  await nav("accept");
  await expect(invert).toHaveAttribute("aria-checked", "true");
  for (let i = 0; i < 40 && !(await save.evaluate((el) => el === document.activeElement)); i++)
    await nav("down");
  await expect(save, await focused()).toBeFocused();
  await nav("accept");
  await expect(page.getByText("Saved")).toBeVisible();
  const calls = (await commandCalls(page, "controllers_profile_set")) as {
    profile: { mapping: { left_stick: { invert_y: boolean } } };
  }[];
  expect(calls.map((call) => call.profile.mapping.left_stick.invert_y)).toEqual([true]);
});
