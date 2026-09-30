// A3-T07 in the browser: Settings → Controllers passes axe (section, tester, mapping dialog) and works
// with the keyboard only and with a controller only.
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";
import { commandCalls, emit, open } from "./helpers";

const pad = (page: Page, action: string) =>
  emit(page, "ui-nav", { action, controller: "xinput", repeat: false });

async function serious(page: Page) {
  const results = await new AxeBuilder({ page }).analyze();
  return results.violations.filter((v) => v.impact === "serious" || v.impact === "critical");
}

async function openSection(page: Page) {
  await open(page, "ready");
  await page.getByRole("link", { name: "Settings" }).click();
  await page.getByRole("link", { name: "Controllers" }).click();
  const region = page.getByRole("region", { name: "Controllers" });
  await expect(region.getByText("DualSense Wireless Controller")).toBeVisible();
  return region;
}

test("the section, the tester and the mapping dialog have no serious violations", async ({
  page,
}) => {
  const region = await openSection(page);
  expect(await serious(page)).toEqual([]);
  await region.getByRole("button", { name: "Start the tester" }).click();
  await emit(page, "controller-input", {
    instance_id: 1,
    pressed: ["south"],
    left: [0, 0],
    right: [0, 0],
    left_trigger: 1000,
    right_trigger: 0,
  });
  await expect(region.getByText(/Pressed: Bottom button/)).toBeVisible();
  expect(await serious(page)).toEqual([]);
  await region.getByRole("button", { name: "Edit the default mapping" }).click();
  const dialog = page.getByRole("dialog", { name: "Default controller mapping" });
  await dialog.getByRole("button", { name: "Change a button" }).click();
  await expect(dialog.getByRole("combobox", { name: "When you press" })).toBeVisible();
  expect(await serious(page)).toEqual([]);
});

test("keyboard only: edit a number, save, focus returns", async ({ page }) => {
  const region = await openSection(page);
  const edit = region.getByRole("button", { name: "Edit the default mapping" });
  await edit.focus();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: "Default controller mapping" });
  const first = dialog.getByRole("textbox", { name: /^Deadzone/ }).first();
  await first.focus();
  await page.keyboard.press("Control+A");
  await page.keyboard.type("12");
  await dialog.getByRole("button", { name: "Save mapping" }).focus();
  await page.keyboard.press("Enter");
  await expect(dialog).toHaveCount(0);
  const saved = (await commandCalls(page, "controller_profile_set")) as {
    profile: { left_stick: { deadzone: number } };
  }[];
  expect(saved[0]?.profile.left_stick.deadzone).toBe(12);
  await expect(edit).toBeFocused();
});

test("controller only: A opens the mapping, B closes it without saving", async ({ page }) => {
  const region = await openSection(page);
  const edit = region.getByRole("button", { name: "Edit the default mapping" });
  await edit.focus();
  await pad(page, "accept");
  const dialog = page.getByRole("dialog", { name: "Default controller mapping" });
  await expect(dialog).toBeVisible();
  await pad(page, "back");
  await expect(dialog).toHaveCount(0);
  expect(await commandCalls(page, "controller_profile_set")).toHaveLength(0);
  await expect(edit).toBeFocused();
});
