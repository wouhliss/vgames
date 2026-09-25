// A3-T04 acceptance: the library with keyboard only, with a controller only, and accessibility.
// Scrolling performance is measured alone in library.perf.spec.ts.
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";
import { commandCalls, emit, open } from "./helpers";

const pad = (page: Page, action: string) =>
  emit(page, "ui-nav", { action, controller: "xinput", repeat: false });

async function focusedName(page: Page): Promise<string> {
  return page.evaluate(() => {
    const el = document.activeElement;
    return el ? (el.getAttribute("aria-label") ?? el.textContent ?? "").trim() : "";
  });
}

test("has no serious accessibility violations (grid, list, dialogs)", async ({ page }) => {
  await open(page, "ready");
  await expect(page.getByRole("tab", { name: "All 40" })).toBeVisible();
  const scan = async () => {
    const results = await new AxeBuilder({ page }).analyze();
    const serious = results.violations.filter(
      (v) => v.impact === "serious" || v.impact === "critical",
    );
    expect(serious, JSON.stringify(serious, null, 2)).toEqual([]);
  };
  await scan();
  await page.getByRole("button", { name: "List" }).click();
  await scan();
  await page.getByRole("button", { name: "Collections…" }).click();
  await expect(page.getByRole("dialog", { name: "Collections" })).toBeVisible();
  await scan();
});

test("works with the keyboard only: search, play, menu, collections", async ({ page }) => {
  await open(page, "ready");
  await expect(page.getByRole("tab", { name: "All 40" })).toBeVisible();
  await page.getByRole("searchbox", { name: "Search your library" }).focus();
  await page.keyboard.type("velvet");
  await expect(page.getByRole("article")).toHaveCount(1);
  // Down from the search field reaches the only tile's main button, then its Play button.
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  const play = page.getByRole("button", { name: /^Play Velvet/ });
  await expect(play).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("button", { name: /^Stop Velvet/ })).toBeVisible();
  expect(await commandCalls(page, "game_launch")).toHaveLength(1);
  // The context menu from the keyboard, then "Add to collection…".
  await page.keyboard.press("Shift+F10");
  const menu = page.getByRole("menu", { name: /^Actions for Velvet/ });
  await expect(menu).toBeVisible();
  await page.keyboard.type("add to c");
  await expect(page.getByRole("menuitem", { name: "Add to collection…" })).toBeFocused();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: /^Add Velvet .* to collections$/ });
  await expect(dialog).toBeVisible();
  await page.keyboard.press("Space");
  await expect(dialog.getByRole("checkbox").first()).toBeChecked();
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await page.keyboard.press("Escape"); // clears the search
});

test("works with a controller only: move through tiles, play, open the menu", async ({ page }) => {
  await open(page, "ready");
  await expect(page.getByRole("tab", { name: "All 40" })).toBeVisible();
  const firstTile = page.getByRole("article").first().getByRole("button").first();
  await firstTile.focus();
  const start = await focusedName(page);
  await pad(page, "right");
  expect(await focusedName(page)).not.toBe(start);
  await pad(page, "left");
  expect(await focusedName(page)).toBe(start);
  // Down reaches Play under the cover; A launches.
  await pad(page, "down");
  expect(await focusedName(page)).toMatch(/^Play /);
  await pad(page, "accept");
  await expect(page.getByRole("button", { name: /^Stop / }).first()).toBeVisible();
  // Y opens the context menu; B closes it and focus returns.
  await pad(page, "menu");
  await expect(page.getByRole("menu")).toBeVisible();
  await pad(page, "back");
  await expect(page.getByRole("menu")).toHaveCount(0);
  // RB switches to the next collection tab.
  await pad(page, "tab_next");
  await expect(page.getByRole("tab", { name: /^Favorites/ })).toHaveAttribute(
    "aria-selected",
    "true",
  );
});
