// INS-06 in the browser: the publish screen passes axe (form, live upload, invalid entries and the
// dialogs), an upload moves pack by pack to "checked" and is released, and players never see it.
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";
import { open } from "./helpers";

async function scan(page: Page): Promise<void> {
  await expect(page.getByRole("status").filter({ hasText: "Loading" })).toHaveCount(0);
  const results = await new AxeBuilder({ page }).analyze();
  const serious = results.violations.filter(
    (v) => v.impact === "serious" || v.impact === "critical",
  );
  expect(serious, JSON.stringify(serious, null, 2)).toEqual([]);
}

async function toPublish(page: Page, state?: Record<string, unknown>): Promise<void> {
  await open(page, "admin", state);
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Publish" })
    .click();
  await expect(page.getByRole("heading", { level: 1, name: "Publish" })).toBeVisible();
}

async function choose(page: Page, field: string, option: RegExp): Promise<void> {
  await page.getByRole("combobox", { name: field }).click();
  await page.getByRole("option", { name: option }).click();
}

async function fillForm(page: Page): Promise<void> {
  await choose(page, "Package", /^Starfall\b/);
  await page.getByRole("button", { name: "Choose folder" }).click();
  await expect(page.getByText(/files · .+ · .+ packs/)).toBeVisible();
  await choose(page, "Platform", /^Windows \(x64\)$/);
  await page.getByRole("textbox", { name: /Version name/ }).fill("1.4.0");
  await page.getByRole("button", { name: "Choose key file" }).click();
  await page.getByLabel("Key passphrase").fill("correct horse");
}

test("is not in the sidebar for players", async ({ page }) => {
  await open(page, "ready");
  const nav = page.getByRole("navigation", { name: "Main" });
  await expect(nav.getByRole("link", { name: "Library" })).toBeVisible();
  await expect(nav.getByRole("link", { name: "Publish" })).toHaveCount(0);
});

test("has no serious accessibility violations (form, dialogs, invalid entries)", async ({
  page,
}) => {
  await toPublish(page);
  await scan(page);
  await page.getByRole("button", { name: "New package" }).click();
  await expect(page.getByRole("dialog", { name: "New package" })).toBeVisible();
  await scan(page);
  await page.getByRole("button", { name: "Cancel" }).click();
  await fillForm(page);
  await scan(page);

  await toPublish(page, { publishFolderPick: "/home/demo/builds/broken-names" });
  await choose(page, "Package", /^Starfall\b/);
  await page.getByRole("button", { name: "Choose folder" }).click();
  await expect(page.getByText("3 files can't be published")).toBeVisible();
  await scan(page);
});

test("uploads pack by pack, gets checked, and is released", async ({ page }) => {
  await toPublish(page);
  await fillForm(page);
  await page.getByRole("button", { name: "Upload" }).click();
  const job = page
    .getByRole("listitem")
    .filter({ has: page.getByRole("heading", { level: 3, name: "Starfall 1.4.0" }) });
  await expect(job.getByText(/^Uploading · .+ of .+/)).toBeVisible();
  await scan(page);
  await expect(job.getByText("Checked, not released").first()).toBeVisible({ timeout: 15_000 });
  await job.getByRole("button", { name: "Release Starfall 1.4.0" }).click();
  const dialog = page.getByRole("alertdialog", { name: "Release Starfall 1.4.0?" });
  await scan(page);
  await dialog.getByRole("button", { name: "Release" }).click();
  await expect(page.getByText("Starfall 1.4.0 is released.")).toBeVisible();
  const versions = page.getByRole("region", { name: "Versions of Starfall" });
  await expect(versions.getByText("Current release").first()).toBeVisible();
});
