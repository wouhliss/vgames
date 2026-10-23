import { expect, type Page, test } from "@playwright/test";
import { axeViolations, open } from "./helpers";
import { ROUTE_SCANS } from "./route-scans";

async function scan(page: Page): Promise<void> {
  const violations = (await axeViolations(page)).filter(
    (v) => v.impact === "serious" || v.impact === "critical",
  );
  expect(violations, JSON.stringify(violations, null, 2)).toEqual([]);
}

async function settled(page: Page): Promise<void> {
  await expect(page.getByRole("status").filter({ hasText: "Loading" })).toHaveCount(0);
}

for (const fixture of ROUTE_SCANS) {
  test(`route ${fixture.pattern} has no serious accessibility violations`, async ({ page }) => {
    await open(page, fixture.preset);
    let path: string = fixture.path;
    let heading: string = fixture.heading;
    if (fixture.pattern === "/package/:packageId") {
      await page.waitForFunction(() => window.__vgamesMock !== undefined);
      const pkg = await page.evaluate(() => window.__vgamesMock?.backend.state.packages[42]);
      if (!pkg) throw new Error("package scan fixture is missing");
      path = `/package/${pkg.id}`;
      heading = pkg.title;
    }
    await page.goto(`${path}?mock=${fixture.preset}`);
    await expect(page.getByRole("heading", { level: 1, name: heading, exact: true })).toBeVisible();
    // The gallery intentionally displays a permanent LoadingState component example.
    if (fixture.pattern !== "/dev/gallery") await settled(page);
    await scan(page);
    if (fixture.pattern === "/settings/*") {
      const links = page.getByRole("navigation", { name: "Settings sections" }).getByRole("link");
      const count = await links.count();
      expect(count).toBeGreaterThan(0);
      for (let i = 0; i < count; i++) {
        const link = links.nth(i);
        const name = await link.innerText();
        await link.click();
        await expect(page.getByRole("region", { name, exact: true })).toBeVisible();
        await settled(page);
        await scan(page);
      }
    }
    if (fixture.pattern === "/friends/*") {
      const tabs = page.getByRole("tab");
      const count = await tabs.count();
      expect(count).toBeGreaterThan(0);
      for (let i = 0; i < count; i++) {
        const tab = tabs.nth(i);
        await tab.click();
        await expect(tab).toHaveAttribute("aria-selected", "true");
        await settled(page);
        await scan(page);
      }
    }
  });
}
