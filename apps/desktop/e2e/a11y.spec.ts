// A3-T12: axe on every launcher screen in every theme (dark, light, high contrast), at "serious" and
// above. Screens with their own states (dialogs, errors) are covered in each feature's spec.
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";
import { open } from "./helpers";

const THEMES = ["dark", "light", "high_contrast"] as const;

const SCREENS: { name: string; path: string; heading: RegExp | string }[] = [
  { name: "library", path: "/library", heading: "Library" },
  { name: "browse", path: "/browse", heading: "Browse" },
  { name: "friends", path: "/friends", heading: "Friends" },
  { name: "friends: add", path: "/friends/add", heading: "Friends" },
  { name: "friends: requests", path: "/friends/requests", heading: "Friends" },
  { name: "downloads", path: "/downloads", heading: "Downloads" },
  { name: "publish", path: "/publish", heading: "Publish" },
  ...[
    "general",
    "servers",
    "account",
    "storage",
    "downloads",
    "compatibility",
    "cloud-saves",
    "controllers",
    "privacy",
    "overlay",
    "updates",
    "about",
  ].map((id) => ({ name: `settings: ${id}`, path: `/settings/${id}`, heading: "Settings" })),
];

async function goTo(page: Page, path: string) {
  await page.evaluate((to) => {
    window.history.pushState({}, "", to);
    window.dispatchEvent(new PopStateEvent("popstate"));
  }, path);
}

for (const theme of THEMES) {
  test(`no serious accessibility violations on any screen (${theme})`, async ({ page }) => {
    test.setTimeout(180_000);
    await open(page, "admin", { appearance: { theme, reduce_motion: true } });
    await expect(page.getByRole("heading", { level: 1, name: "Library" })).toBeVisible();
    const failures: string[] = [];
    for (const screen of SCREENS) {
      await goTo(page, screen.path);
      await expect(page.getByRole("heading", { level: 1, name: screen.heading })).toBeVisible();
      // Let data load and the first paint settle before scanning.
      await page.waitForTimeout(250);
      const results = await new AxeBuilder({ page }).analyze();
      for (const v of results.violations) {
        if (v.impact === "serious" || v.impact === "critical") {
          failures.push(`${screen.name}: ${v.id} (${v.impact}) — ${v.nodes.length} node(s)`);
        }
      }
    }
    expect(failures).toEqual([]);
  });
}
