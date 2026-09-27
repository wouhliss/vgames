// A3-T07 acceptance in the browser: every settings section passes axe, and settings work with the
// keyboard only and with a controller only.
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";
import { emit, open } from "./helpers";

const pad = (page: Page, action: string) =>
  emit(page, "ui-nav", { action, controller: "xinput", repeat: false });

async function toSettings(page: Page): Promise<void> {
  await open(page, "ready");
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Settings" })
    .click();
  await expect(page.getByRole("region", { name: "General" })).toBeVisible();
}

const sections = (page: Page) => page.getByRole("navigation", { name: "Settings sections" });

test("every section has no serious accessibility violations", async ({ page }) => {
  await toSettings(page);
  for (const name of [
    "General",
    "Servers",
    "Account",
    "Storage",
    "Downloads",
    "Updates",
    "About",
  ]) {
    await sections(page).getByRole("link", { name }).click();
    await expect(page.getByRole("region", { name })).toBeVisible();
    // Wait for the section's data before scanning.
    await expect(page.getByRole("status").filter({ hasText: "Loading" })).toHaveCount(0);
    const results = await new AxeBuilder({ page }).analyze();
    const serious = results.violations.filter(
      (v) => v.impact === "serious" || v.impact === "critical",
    );
    expect(serious, `${name}: ${JSON.stringify(serious, null, 2)}`).toEqual([]);
  }
});

test("works with the keyboard only: pick a section, change the theme", async ({ page }) => {
  await toSettings(page);
  const general = sections(page).getByRole("link", { name: "General" });
  await general.focus();
  // Down moves through the section list; Enter opens a section.
  await page.keyboard.press("ArrowDown");
  await expect(sections(page).getByRole("link", { name: "Servers" })).toBeFocused();
  await page.keyboard.press("ArrowUp");
  await expect(general).toBeFocused();
  await page.keyboard.press("Enter");
  // Into the theme choice: arrows select within the group.
  const themes = page.getByRole("radiogroup", { name: "Theme" });
  await themes.getByRole("radio", { checked: true }).focus();
  await page.keyboard.press("ArrowDown");
  await expect(themes.getByRole("radio", { checked: true })).toBeFocused();
  const theme = await page.evaluate(() => document.documentElement.dataset.theme);
  expect(theme).toBeTruthy();
  const saved = await page.evaluate(() => window.__vgamesMock?.backend.state.appearance.theme);
  expect(saved).not.toBe("dark");
});

test("works with a controller only: move through sections and toggle a setting", async ({
  page,
}) => {
  await toSettings(page);
  await sections(page).getByRole("link", { name: "General" }).focus();
  for (let i = 0; i < 4; i += 1) await pad(page, "down");
  await expect(sections(page).getByRole("link", { name: "Downloads" })).toBeFocused();
  await pad(page, "accept");
  const limit = page.getByRole("switch", { name: "Limit download speed" });
  await expect(limit).toBeVisible();
  await limit.focus();
  await pad(page, "accept");
  await expect(limit).toHaveAttribute("aria-checked", "true");
  await expect(page.getByRole("textbox", { name: "Maximum speed (MB/s)" })).toHaveValue("10");
});
