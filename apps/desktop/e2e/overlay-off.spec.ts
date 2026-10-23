// GAME-11 in the browser: the notice the crash safety valve raises passes axe and works with the
// keyboard only. The `ready` preset has 40 installed games.
import { expect, type Page, test } from "@playwright/test";
import { axeViolations, commandCalls, emit, open } from "./helpers";

async function firstGame(page: Page): Promise<{ title: string; package: unknown }> {
  await page.waitForFunction(() => window.__vgamesMock !== undefined);
  return page.evaluate(() => {
    const game = window.__vgamesMock?.backend.state.installs[0];
    if (!game) throw new Error("no installs in the preset");
    return { title: game.title, package: game.package };
  });
}

test("the overlay-off notice names the game, passes axe and turns the overlay back on", async ({
  page,
}) => {
  await open(page, "ready");
  await expect(page.getByRole("heading", { level: 1, name: "Library" })).toBeVisible();
  const game = await firstGame(page);
  await emit(page, "overlay-package-disabled", { package: game.package });
  const notice = page.getByRole("region", { name: "In-game overlay turned off" });
  await expect(notice).toContainText(`The in-game overlay is off for ${game.title}.`);

  const violations = await axeViolations(page, "section[aria-label]");
  const serious = violations.filter((v) => v.impact === "serious" || v.impact === "critical");
  expect(serious).toEqual([]);

  const turnOn = notice.getByRole("button", { name: "Turn it back on" });
  await turnOn.focus();
  await page.keyboard.press("Enter");
  await expect(notice).toBeHidden();
  expect(await commandCalls(page, "package_overlay_set")).toEqual([
    { packageRef: game.package, enabled: true },
  ]);
});
