// A3-T08 in the browser: the conflict dialog and Settings → Cloud saves pass axe and work with the
// keyboard only and with a controller only. The `ready` preset has a game whose saves conflict.
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";
import { commandCalls, emit, open } from "./helpers";

const pad = (page: Page, action: string) =>
  emit(page, "ui-nav", { action, controller: "xinput", repeat: false });

async function serious(page: Page) {
  const results = await new AxeBuilder({ page }).analyze();
  return results.violations.filter((v) => v.impact === "serious" || v.impact === "critical");
}

const dialog = (page: Page) =>
  page.getByRole("dialog", { name: "Which saves do you want to keep?" });

async function openConflict(page: Page) {
  await open(page, "ready");
  await page.getByRole("link", { name: "Settings" }).click();
  await page.getByRole("link", { name: "Cloud saves" }).click();
  const region = page.getByRole("region", { name: "Cloud saves" });
  const resolve = region.getByRole("button", { name: /^Resolve the saves conflict of / }).first();
  await expect(resolve).toBeVisible();
  return resolve;
}

test("the conflict dialog and the cloud saves list have no serious accessibility violations", async ({
  page,
}) => {
  const resolve = await openConflict(page);
  expect(await serious(page)).toEqual([]);
  await resolve.click();
  await expect(dialog(page).getByRole("radio")).toHaveCount(3);
  expect(await serious(page)).toEqual([]);
});

test("the history dialog has no serious accessibility violations", async ({ page }) => {
  await openConflict(page);
  await page
    .getByRole("region", { name: "Cloud saves" })
    .getByRole("button", { name: /^Saves history of / })
    .first()
    .click();
  await expect(page.getByRole("dialog", { name: /^Saves history of / })).toBeVisible();
  await expect(page.getByText("In the cloud", { exact: true })).toBeVisible();
  expect(await serious(page)).toEqual([]);
});

test("keyboard only: choose, continue, and nothing is chosen until then", async ({ page }) => {
  const resolve = await openConflict(page);
  await resolve.focus();
  await page.keyboard.press("Enter");
  // Focus starts on the choices, and Continue does nothing until one is selected.
  await expect(dialog(page).getByRole("radio").first()).toBeFocused();
  await expect(dialog(page).getByRole("button", { name: "Continue" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  expect(await commandCalls(page, "saves_resolve")).toHaveLength(0);
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  await expect(dialog(page).getByRole("radio", { name: /^Keep both/ })).toBeChecked();
  await dialog(page).getByRole("button", { name: "Continue" }).focus();
  await page.keyboard.press("Enter");
  await expect(dialog(page)).toHaveCount(0);
  expect(await commandCalls(page, "saves_resolve")).toHaveLength(1);
  // One of the two conflicts in the fixture is settled; the other still waits for its player.
  await expect(
    page
      .getByRole("region", { name: "Cloud saves" })
      .getByRole("button", { name: /^Resolve the saves conflict of / }),
  ).toHaveCount(1);
});

test("controller only: B cancels the dialog and sends nothing", async ({ page }) => {
  const resolve = await openConflict(page);
  await resolve.focus();
  await pad(page, "accept");
  await expect(dialog(page)).toBeVisible();
  await pad(page, "back");
  await expect(dialog(page)).toHaveCount(0);
  expect(await commandCalls(page, "saves_resolve")).toHaveLength(0);
  await expect(resolve).toBeFocused();
});
