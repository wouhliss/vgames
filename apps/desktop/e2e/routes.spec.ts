// INT-10: adding a production route requires an explicit accessible screen case.
import { readFileSync } from "node:fs";
import { expect, test } from "@playwright/test";
import { axeScan, emit, open } from "./helpers";

const cases = [
  { route: "onboarding", preset: "fresh", heading: "Welcome to vgames" },
  { route: "library", preset: "ready", heading: "Library" },
  { route: "browse", preset: "ready", heading: "Browse" },
  { route: "package/:packageId", preset: "ready", heading: "" },
  { route: "friends/*", preset: "ready", heading: "Friends" },
  { route: "downloads", preset: "ready", heading: "Downloads" },
  { route: "settings/*", preset: "ready", heading: "Settings" },
];

test("every production screen has an accessibility case", () => {
  const source = readFileSync("src/app/router.tsx", "utf8");
  const properties = [...source.matchAll(/\bpath\s*:/g)];
  const paths = [...source.matchAll(/\bpath:\s*"([^"]+)"/g)].map((match) => match[1] ?? "");
  // Reject computed/shorthand paths instead of silently omitting them from coverage.
  expect(paths.length, "Computed route paths need an explicit coverage case").toBe(
    properties.length,
  );
  expect(source, "Shorthand route paths need an explicit coverage case").not.toMatch(
    /\bpath\s*[,}]/,
  );
  expect(paths.filter((route) => !["/", "*", "dev/gallery"].includes(route)).sort()).toEqual(
    cases.map((item) => item.route).sort(),
  );
});

for (const mode of ["keyboard", "controller"] as const) {
  for (const item of cases) {
    test(`${mode}: screen ${item.route} passes the shared axe scan`, async ({ page }) => {
      const activate = async () => {
        if (mode === "keyboard") await page.keyboard.press("Enter");
        else await emit(page, "ui-nav", { action: "accept", controller: "xinput", repeat: false });
      };
      await open(page, item.preset);
      if (item.route === "package/:packageId") {
        await page
          .getByRole("navigation", { name: "Main" })
          .getByRole("link", { name: "Browse" })
          .focus();
        await activate();
        await expect(page.getByRole("heading", { level: 1, name: "Browse" })).toBeVisible();
        await page.getByTestId("catalog-grid").getByRole("link").first().focus();
        await activate();
        await expect(page).toHaveURL(/\/package\//);
      } else if (item.preset !== "fresh") {
        await page
          .getByRole("navigation", { name: "Main" })
          .getByRole("link", { name: item.heading })
          .focus();
        await activate();
      }
      if (item.heading)
        await expect(page.getByRole("heading", { level: 1, name: item.heading })).toBeVisible();
      await expect(page.getByRole("status").filter({ hasText: "Loading" })).toHaveCount(0);
      await axeScan(page);
    });
  }
}
