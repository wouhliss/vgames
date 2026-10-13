import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";
import { emit, open } from "./helpers";

async function editor(page: Page, game?: string) {
  await open(page, "ready", { persistKey: "controller-profile-browser" });
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Settings" })
    .click();
  await page
    .getByRole("navigation", { name: "Settings sections" })
    .getByRole("link", { name: "Controllers" })
    .click();
  await page.getByRole("combobox", { name: "Game", exact: true }).click();
  const option = game
    ? page.getByRole("option", { name: game, exact: true })
    : page.getByRole("option").first();
  const name = await option.innerText();
  await option.click();
  await expect(page.getByRole("button", { name: "Save controller profile" })).toBeVisible();
  return name;
}

test("complete controller profile is axe clean and keyboard changes survive restart", async ({
  page,
}) => {
  const game = await editor(page);
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  await page.getByRole("radio", { name: "Never", exact: true }).focus();
  await page.keyboard.press("Space");
  const slider = page.getByRole("slider", { name: "Left stick deadzone", exact: true });
  await slider.focus();
  await page.keyboard.press("ArrowRight");
  await expect(slider).toHaveValue("9");
  const remap = page.getByRole("combobox", { name: "Remap Bottom face button" });
  await remap.focus();
  await page.keyboard.press("Enter");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(remap).toHaveText("Disable button");
  await page.getByRole("button", { name: "Save controller profile" }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("status").filter({ hasText: "Saved" })).toBeVisible();
  await editor(page, game);
  await expect(page.getByRole("radio", { name: "Never", exact: true })).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await expect(slider).toHaveValue("9");
  await expect(remap).toHaveText("Disable button");
});

test("gamepad controls edit and save a profile without pointer input", async ({ page }) => {
  await editor(page);
  const nav = (action: string) =>
    emit(page, "ui-nav", { action, controller: "xinput", repeat: false });
  const modes = page.getByRole("radiogroup", { name: "Controller emulation" });
  await modes.getByRole("radio", { name: "Automatic" }).focus();
  await nav("right");
  await expect(modes.getByRole("radio", { name: "Always", exact: true })).toHaveAttribute(
    "aria-checked",
    "true",
  );
  const slider = page.getByRole("slider", { name: "Right stick anti-deadzone" });
  await slider.focus();
  await nav("right");
  await expect(slider).toHaveValue("1");
  await expect(slider).toBeFocused();
  const remap = page.getByRole("combobox", { name: "Remap Guide / PS" });
  await remap.focus();
  await nav("accept");
  await nav("down");
  await nav("accept");
  await expect(remap).toHaveText("Disable button");
  await page.getByRole("button", { name: "Save controller profile" }).focus();
  await nav("accept");
  await expect(page.getByRole("status").filter({ hasText: "Saved" })).toBeVisible();
});
