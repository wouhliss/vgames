// A3-T10 in the browser: the update banner and What's new pass axe, and work with the keyboard only
// and with a controller only. The `ready` preset has 0.9.1 available over the installed 0.4.0.
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";
import { commandCalls, emit, open } from "./helpers";

const pad = (page: Page, action: string) =>
  emit(page, "ui-nav", { action, controller: "xinput", repeat: false });

async function serious(page: Page) {
  const results = await new AxeBuilder({ page }).analyze();
  return results.violations.filter((v) => v.impact === "serious" || v.impact === "critical");
}

const banner = (page: Page) => page.getByRole("region", { name: "vgames 0.9.1 is available" });
/** Focuses the banner's "What's new" once the first screen has settled (it takes focus on arrival). */
async function focusWhatsNew(page: Page) {
  const button = banner(page).getByRole("button", { name: "What's new" });
  await expect(page.getByRole("heading", { level: 1, name: "Library" })).toBeVisible();
  await expect(async () => {
    await button.focus();
    await expect(button).toBeFocused({ timeout: 200 });
  }).toPass();
  return button;
}

const dialog = (page: Page) => page.getByRole("dialog", { name: "What's new in vgames 0.9.1" });

test("banner and What's new have no serious accessibility violations", async ({ page }) => {
  await open(page, "ready");
  await expect(banner(page)).toBeVisible();
  expect(await serious(page)).toEqual([]);
  await banner(page).getByRole("button", { name: "What's new" }).click();
  await expect(dialog(page).getByRole("region", { name: "What's new" })).toBeVisible();
  expect(await serious(page)).toEqual([]);
});

test("keyboard only: open What's new, scroll it, install and restart", async ({ page }) => {
  await open(page, "ready");
  await focusWhatsNew(page);
  await page.keyboard.press("Enter");
  const notes = dialog(page).getByRole("region", { name: "What's new" });
  await expect(notes).toBeFocused();
  await expect(notes.getByRole("heading", { name: "Security" })).toBeVisible();
  await page.keyboard.press("Tab");
  await page.keyboard.press("Tab");
  const install = dialog(page).getByRole("button", { name: "Install and restart" });
  await expect(install).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByText("Downloading vgames 0.9.1…")).toBeVisible();
  await expect(page.getByText("vgames 0.9.1 is installed. Restarting…")).toBeVisible();
  expect(await commandCalls(page, "updater_install")).toHaveLength(1);
});

test("keyboard only: Later closes the dialog and returns focus", async ({ page }) => {
  await open(page, "ready");
  const whatsNew = await focusWhatsNew(page);
  await page.keyboard.press("Enter");
  await expect(dialog(page)).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog(page)).toHaveCount(0);
  await expect(whatsNew).toBeFocused();
});

test("controller only: A opens What's new, B closes it", async ({ page }) => {
  await open(page, "ready");
  const whatsNew = await focusWhatsNew(page);
  await pad(page, "accept");
  await expect(dialog(page)).toBeVisible();
  await pad(page, "back");
  await expect(dialog(page)).toHaveCount(0);
  await expect(whatsNew).toBeFocused();
});

test("a long changelog scrolls inside the dialog", async ({ page }) => {
  const releases = Array.from({ length: 20 }, (_, i) => ({
    version: `0.9.${20 - i}`,
    date: "2026-09-01",
    entries: Array.from({ length: 10 }, (_, j) => ({ type: "fixed", text: `Fixed ${i}-${j}.` })),
  }));
  await open(page, "ready", { whatsNew: { from_latest_notes: false, releases } });
  await banner(page).getByRole("button", { name: "What's new" }).click();
  const notes = dialog(page).getByRole("region", { name: "What's new" });
  await expect(notes).toBeFocused();
  const box = await notes.evaluate((el) => ({ scroll: el.scrollHeight, client: el.clientHeight }));
  expect(box.scroll).toBeGreaterThan(box.client);
  await page.keyboard.press("End");
  await expect(notes.getByText("Fixed 19-9.")).toBeInViewport();
  await expect(dialog(page).getByRole("button", { name: "Install and restart" })).toBeInViewport();
});
