// A3-T05 acceptance: Browse and package details with keyboard only, with a controller only, and
// accessibility of the grid, the details page, the install dialog and the screenshot viewer.
import { expect, type Locator, type Page, test } from "@playwright/test";
import { axeScan, commandCalls, emit, open } from "./helpers";

const pad = (page: Page, action: string) =>
  emit(page, "ui-nav", { action, controller: "xinput", repeat: false });

/** Catalog fixture `i` (see src/mocks/catalog.ts): 13 runs with Proton, 42 is native with 5 shots. */
async function fixtureTitle(page: Page, index: number): Promise<string> {
  await page.waitForFunction(() => window.__vgamesMock !== undefined);
  return page.evaluate((i) => window.__vgamesMock?.backend.state.packages[i]?.title ?? "", index);
}

async function toBrowse(page: Page, preset: string): Promise<void> {
  await open(page, preset);
  await page.getByRole("link", { name: "Browse" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Browse" })).toBeVisible();
  await expect(page.getByTestId("catalog-grid").getByRole("link").first()).toBeVisible();
}

/** Presses Tab until `target` has focus (a few presses at most). */
async function tabTo(page: Page, target: Locator): Promise<void> {
  for (let i = 0; i < 6; i += 1) {
    if (await target.evaluate((el) => el === document.activeElement)) return;
    await page.keyboard.press("Tab");
  }
  await expect(target).toBeFocused();
}

async function focusedName(page: Page): Promise<string> {
  return page.evaluate(() => {
    const el = document.activeElement;
    if (!el) return "";
    const labelledBy = el.getAttribute("aria-labelledby");
    const label = labelledBy ? document.getElementById(labelledBy)?.textContent : null;
    return (label ?? el.getAttribute("aria-label") ?? el.textContent ?? "").trim();
  });
}

test("has no serious accessibility violations (grid, details, install, screenshots)", async ({
  page,
}) => {
  await toBrowse(page, "empty");
  const scan = async () => {
    const results = await axeScan(page);
    const serious = results.violations.filter(
      (v) => v.impact === "serious" || v.impact === "critical",
    );
    expect(serious, JSON.stringify(serious, null, 2)).toEqual([]);
  };
  await scan();
  const title = await fixtureTitle(page, 13);
  await page.getByRole("searchbox", { name: "Search the catalog" }).fill(title);
  await page.getByRole("link", { name: title }).click();
  await expect(page.getByRole("heading", { level: 1, name: title })).toBeVisible();
  await expect(page.getByRole("region", { name: "Compatibility" })).toContainText(
    "Runs with Proton",
  );
  await scan();
  await page.getByRole("button", { name: `Install ${title}` }).click();
  await expect(
    page.getByRole("dialog", { name: `Install ${title}` }).getByRole("radiogroup"),
  ).toBeVisible();
  await scan();
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: /^Screenshot 1 of/ }).click();
  await expect(page.getByRole("dialog", { name: `Screenshots of ${title}` })).toBeVisible();
  await scan();
});

test("works with the keyboard only: search, open, install", async ({ page }) => {
  await toBrowse(page, "ready");
  const title = await fixtureTitle(page, 42);
  const search = page.getByRole("searchbox", { name: "Search the catalog" });
  await search.focus();
  await page.keyboard.type(title);
  await expect(page.getByTestId("catalog-grid").getByRole("link")).toHaveCount(1);
  // Down from the search field reaches the only card; Enter opens it.
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  await expect(page.getByRole("link", { name: title })).toBeFocused();
  await page.keyboard.press("Enter");
  const heading = page.getByRole("heading", { level: 1, name: title });
  await expect(heading).toBeFocused();

  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: `Install ${title}` })).toBeFocused();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: `Install ${title}` });
  const home = dialog.getByRole("radio", { name: /\/home\/sam\/Games/ });
  await expect(home).toBeFocused();
  await expect(home).toHaveAttribute("aria-checked", "true");
  // The offline drive can't be picked: arrows skip it.
  await page.keyboard.press("ArrowDown");
  await expect(home).toHaveAttribute("aria-checked", "true");
  await expect(dialog.getByRole("radio", { name: /External drive/ })).toHaveAttribute(
    "aria-checked",
    "false",
  );
  await tabTo(page, dialog.getByRole("button", { name: "Install" }));
  await page.keyboard.press("Enter");
  await expect(page.getByText(`${title} is queued`)).toBeVisible();
  await expect(dialog).toHaveCount(0);
  const calls = (await commandCalls(page, "install_start")) as { libraryId: string }[];
  expect(calls).toHaveLength(1);
  expect(calls[0]?.libraryId).toBe("01920000-0000-7000-8000-0000000000b1");
  await expect(page.getByRole("button", { name: "View download", exact: true })).toBeVisible();
});

test("works with a controller only: cards, details, screenshots, back", async ({ page }) => {
  await toBrowse(page, "ready");
  const first = page.getByTestId("catalog-grid").getByRole("link").first();
  await first.focus();
  const start = await focusedName(page);
  await pad(page, "right");
  expect(await focusedName(page)).not.toBe(start);
  await pad(page, "left");
  expect(await focusedName(page)).toBe(start);
  // Nothing around the focused card may clip its focus ring.
  const clipper = await page.evaluate(() => {
    let el = document.activeElement?.parentElement ?? null;
    while (el && el.id !== "main-content") {
      const style = getComputedStyle(el);
      if (
        style.contain.includes("paint") ||
        style.contain.includes("strict") ||
        style.overflow !== "visible"
      )
        return el.className || el.tagName;
      el = el.parentElement;
    }
    return null;
  });
  expect(clipper).toBeNull();

  // A opens the details page of the focused card.
  await pad(page, "accept");
  await expect(page.getByRole("heading", { level: 1, name: start })).toBeVisible();

  // Screenshots: A opens the viewer, RB and the D-pad step, B closes it and focus comes back.
  const title = await fixtureTitle(page, 42);
  await page.goBack();
  await page.getByRole("searchbox", { name: "Search the catalog" }).fill(title);
  await page.getByRole("link", { name: title }).click();
  const thumb = page.getByRole("button", { name: "Screenshot 1 of 5" });
  await thumb.focus();
  await pad(page, "accept");
  const viewer = page.getByRole("dialog", { name: `Screenshots of ${title}` });
  await expect(viewer).toHaveAccessibleDescription("1 of 5");
  await pad(page, "tab_next");
  await expect(viewer).toHaveAccessibleDescription("2 of 5");
  await pad(page, "right");
  await expect(viewer).toHaveAccessibleDescription("3 of 5");
  await pad(page, "tab_prev");
  await pad(page, "tab_prev");
  await pad(page, "tab_prev");
  await expect(viewer).toHaveAccessibleDescription("5 of 5");
  await pad(page, "back");
  await expect(viewer).toHaveCount(0);
  await expect(thumb).toBeFocused();

  // B on the page goes back to Browse.
  await pad(page, "back");
  await expect(page.getByRole("heading", { level: 1, name: "Browse" })).toBeVisible();
});

test("shows removed packages and missing builds plainly", async ({ page }) => {
  await toBrowse(page, "empty");
  // Fixture 47 only has a Mac build: not available on this (Linux) computer.
  const macOnly = await fixtureTitle(page, 47);
  await page.getByRole("searchbox", { name: "Search the catalog" }).fill(macOnly);
  const card = page.getByRole("link", { name: macOnly });
  await expect(card).toHaveAccessibleDescription("Mac Not available on this computer");
  await card.click();
  await expect(
    page.getByRole("button", { name: "Not available on this computer" }),
  ).toHaveAttribute("aria-disabled", "true");

  // The admins unpublish it while the page is open: installing says so, then the page does too.
  const title = await fixtureTitle(page, 42);
  await page.goBack();
  await page.getByRole("searchbox", { name: "Search the catalog" }).fill(title);
  await page.getByRole("link", { name: title }).click();
  await expect(page.getByRole("heading", { level: 1, name: title })).toBeVisible();
  await page.evaluate(() => {
    const mock = window.__vgamesMock;
    const id = mock?.backend.state.packages[42]?.id;
    if (mock && id) mock.backend.state.removedPackages = [id];
  });
  await page.getByRole("button", { name: `Install ${title}` }).click();
  const dialog = page.getByRole("dialog", { name: `Install ${title}` });
  await expect(dialog.getByRole("alert")).toHaveText("This package was removed from the server.");
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(
    page.getByRole("heading", { name: "This package isn't available anymore" }),
  ).toBeVisible();
});
