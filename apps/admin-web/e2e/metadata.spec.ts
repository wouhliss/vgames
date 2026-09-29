// A3-T15 in the browser (mock server): metadata review and images, including a real multipart upload
// with progress through the service worker. Mock-only: it drives the mock server's lookup outcome.
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";

test.skip(Boolean(process.env.ADMIN_E2E_BASE_URL), "drives the mock server");

const HARBOR = "01920000-0000-7000-8000-0000000b0001";
const CANYON = "01920000-0000-7000-8000-0000000b0002";
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mN8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==",
  "base64",
);

async function serious(page: Page) {
  const results = await new AxeBuilder({ page }).analyze();
  return results.violations.filter((v) => v.impact === "serious" || v.impact === "critical");
}

test("metadata and images tabs have no serious accessibility violations", async ({ page }) => {
  await page.goto(`/admin/packages/${HARBOR}/metadata?mock=admin`);
  await page.getByRole("radio", { name: /Compare Hollow Harbor \(IGDB/ }).check();
  await expect(page.getByRole("region", { name: /Compare with IGDB/ })).toBeVisible();
  expect(await serious(page)).toEqual([]);
  await page
    .getByRole("navigation", { name: "Package sections" })
    .getByRole("link", { name: "Images" })
    .click();
  await expect(page.getByRole("img", { name: "Cover of Hollow Harbor" })).toBeVisible();
  expect(await serious(page)).toEqual([]);
});

test("a failed lookup shows its error, and retrying runs it again", async ({ page }) => {
  await page.goto(`/admin/packages/${CANYON}/metadata?mock=admin`);
  await expect(page.getByText(/No candidates found/)).toBeVisible();
  await page.evaluate((id) => {
    const m = window.__adminMock;
    if (m) m.db.lookupOutcome[id] = "fail";
  }, CANYON);
  await page.getByRole("button", { name: "Look up again" }).click();
  await expect(page.getByText("The lookup failed and gave up.", { exact: false })).toBeVisible({
    timeout: 10_000,
  });
  await page.evaluate((id) => {
    const m = window.__adminMock;
    if (m) m.db.lookupOutcome[id] = "succeed";
  }, CANYON);
  await page.getByRole("button", { name: "Try again" }).click();
  await expect(page.getByText("Lookup finished.")).toBeVisible({ timeout: 10_000 });
  await expect(page.getByRole("radio", { name: /Compare/ }).first()).toBeVisible();
});

test("apply conflict (412): reload, then apply", async ({ page }) => {
  await page.goto(`/admin/packages/${HARBOR}/metadata?mock=admin`);
  await page.getByRole("radio", { name: /Compare Hollow Harbor \(IGDB/ }).check();
  await page.evaluate((id) => {
    const m = window.__adminMock;
    if (m) m.editElsewhere(m.db, id, { summary: "Edited meanwhile" });
  }, HARBOR);
  const compare = page.getByRole("region", { name: /Compare with IGDB/ });
  await compare.getByRole("button", { name: /^Apply \d+ fields?$/ }).click();
  await expect(compare.getByText("Changed by someone else")).toBeVisible();
  await compare.getByRole("button", { name: "Reload package" }).click();
  await expect(compare.getByRole("row", { name: /Summary/ })).toContainText("Edited meanwhile");
  await compare.getByRole("button", { name: /^Apply \d+ fields?$/ }).click();
  await expect(page.getByText(/^Applied \d+ fields?/)).toBeVisible();
});

test("uploads: too big and wrong type are refused locally; a real upload shows progress and is used", async ({
  page,
}) => {
  await page.goto(`/admin/packages/${HARBOR}/images?mock=admin`);
  const logo = page.getByRole("region", { name: "Logo" });
  const input = logo.getByLabel("Upload a new logo");
  await input.setInputFiles({
    name: "huge.png",
    mimeType: "image/png",
    buffer: Buffer.concat([PNG, Buffer.alloc(10 * 1024 * 1024)]),
  });
  await expect(logo.getByText(/images can be at most 10 MiB/)).toBeVisible();
  await input.setInputFiles({
    name: "notes.png",
    mimeType: "image/png",
    buffer: Buffer.from("hello"),
  });
  await expect(logo.getByText("Only JPEG, PNG and WebP images can be uploaded.")).toBeVisible();
  await expect(logo.getByRole("button", { name: "Upload", exact: true })).toBeDisabled();

  const posts: string[] = [];
  page.on("request", (r) => {
    if (r.method() === "POST" && r.url().includes("/assets")) posts.push(r.url());
  });
  await input.setInputFiles({ name: "logo.png", mimeType: "image/png", buffer: PNG });
  await logo.getByRole("button", { name: "Upload", exact: true }).click();
  await expect(page.getByText("New logo uploaded and in use.")).toBeVisible();
  await expect(logo.getByRole("img", { name: "Logo of Hollow Harbor" })).toBeVisible();
  expect(posts).toHaveLength(1);
});

test("a broken image shows a placeholder; deleting asks first", async ({ page }) => {
  await page.goto(`/admin/packages/${HARBOR}/images?mock=admin`);
  await expect(
    page.getByRole("img", { name: "Screenshot 1 of Hollow Harbor (couldn't be loaded)" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Delete screenshot 2…" }).click();
  const dialog = page.getByRole("alertdialog", { name: "Delete screenshot 2?" });
  await expect(dialog.getByRole("button", { name: "Cancel" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await page.getByRole("button", { name: "Delete screenshot 2…" }).click();
  await dialog.getByRole("button", { name: "Delete" }).click();
  await expect(page.getByText("Screenshot 2 deleted.")).toBeVisible();
});
