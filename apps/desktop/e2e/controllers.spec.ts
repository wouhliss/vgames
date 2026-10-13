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
