// A3-T11 in the browser: the Publish screen (admins) passes axe at every stage and runs with the keyboard
// only. The `admin` preset signs in as an admin and simulates the upload step by step.
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";
import { commandCalls, open } from "./helpers";

async function serious(page: Page) {
  const results = await new AxeBuilder({ page }).analyze();
  return results.violations.filter((v) => v.impact === "serious" || v.impact === "critical");
}

async function openPublish(page: Page) {
  await open(page, "admin");
  await page.getByRole("link", { name: "Publish" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Publish" })).toBeVisible();
}

async function fillForm(page: Page) {
  await page.getByRole("radio", { name: /Hollow Harbor/ }).click();
  await page.getByRole("button", { name: "Choose folder…" }).click();
  await expect(page.getByText(/files · .* · 6 packs/)).toBeVisible();
  await page.getByRole("textbox", { name: /Version name/ }).fill("2.0.0");
  await page.getByRole("button", { name: "Choose key file…" }).click();
  await page.getByLabel(/^Passphrase/).fill("correct horse battery");
}

test("the form, the running job and the published result have no serious violations", async ({
  page,
}) => {
  await openPublish(page);
  await fillForm(page);
  expect(await serious(page)).toEqual([]);
  await page.getByRole("button", { name: "Upload and verify" }).click();
  await expect(page.getByRole("heading", { name: "Publishing 2.0.0" })).toBeVisible();
  expect(await serious(page)).toEqual([]);
  await expect(page.getByText("Verified. Ready to publish.")).toBeVisible();
  await page.getByRole("button", { name: "Publish version" }).click();
  await expect(page.getByRole("alertdialog", { name: "Publish 2.0.0?" })).toBeVisible();
  expect(await serious(page)).toEqual([]);
  await page.getByRole("alertdialog").getByRole("button", { name: "Publish" }).click();
  await expect(page.getByText("Published.")).toBeVisible();
});

test("keyboard only: fill the form, start, and publish", async ({ page }) => {
  await openPublish(page);
  await page.getByRole("radio", { name: /Hollow Harbor/ }).focus();
  await page.keyboard.press("Space");
  await page.getByRole("button", { name: "Choose folder…" }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByText(/files · .* · 6 packs/)).toBeVisible();
  await page.getByRole("textbox", { name: /Version name/ }).focus();
  await page.keyboard.type("2.0.1");
  await page.getByRole("button", { name: "Choose key file…" }).focus();
  await page.keyboard.press("Enter");
  await page.getByLabel(/^Passphrase/).focus();
  await page.keyboard.type("correct horse battery");
  await page.getByRole("button", { name: "Upload and verify" }).focus();
  await page.keyboard.press("Enter");
  const publish = page.getByRole("button", { name: "Publish version" });
  await expect(publish).toBeVisible();
  await publish.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("alertdialog", { name: "Publish 2.0.1?" })).toBeVisible();
  await page.keyboard.press("Escape");
  expect(await commandCalls(page, "publish_publish")).toHaveLength(0);
  await expect(publish).toBeFocused();
  await page.keyboard.press("Enter");
  await page.getByRole("alertdialog").getByRole("button", { name: "Publish" }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByText("Published.")).toBeVisible();
  expect(await commandCalls(page, "publish_publish")).toHaveLength(1);
});

test("a player has no Publish entry", async ({ page }) => {
  await open(page, "ready");
  await expect(page.getByRole("heading", { level: 1, name: "Library" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Publish" })).toHaveCount(0);
});
