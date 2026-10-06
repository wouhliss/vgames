// A3-T14 acceptance in the browser: packages list, create and editor. Runs against the MSW mock build
// by default; with ADMIN_E2E_BASE_URL against the real stack (the concurrent edit then comes from a
// second browser page instead of the mock server).
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";
import { signIn } from "./helpers";

const external = Boolean(process.env.ADMIN_E2E_BASE_URL);
const mock = external ? "" : "?mock=admin";

test.beforeEach(async ({ page }) => {
  await signIn(page);
});

async function serious(page: Page) {
  const results = await new AxeBuilder({ page }).analyze();
  return results.violations.filter((v) => v.impact === "serious" || v.impact === "critical");
}

async function createPackage(page: Page, title: string): Promise<string> {
  await page.goto(`/admin/packages/new${mock}`);
  await page.getByLabel("Title (required)").fill(title);
  await page.getByRole("button", { name: "Create package" }).click();
  await expect(page.getByRole("heading", { level: 1, name: title })).toBeVisible();
  return page.url();
}

test("list, create and editor have no serious accessibility violations", async ({ page }) => {
  if (external) await createPackage(page, `List fixture ${Date.now()}`);
  await page.goto(`/admin/packages${mock}`);
  await expect(page.getByRole("table")).toBeVisible();
  expect(await serious(page)).toEqual([]);
  await page.getByRole("link", { name: "Create package" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Create package" })).toBeVisible();
  expect(await serious(page)).toEqual([]);
  await createPackage(page, `Axe check ${Date.now()}`);
  expect(await serious(page)).toEqual([]);
});

test("double-clicking Create makes exactly one package", async ({ page }) => {
  const title = `Double click ${Date.now()}`;
  await page.goto(`/admin/packages/new${mock}`);
  await page.getByLabel("Title (required)").fill(title);
  const posts: string[] = [];
  page.on("request", (r) => {
    if (r.method() === "POST" && r.url().endsWith("/v1/admin/packages"))
      posts.push(r.headers()["idempotency-key"] ?? "");
  });
  await page.getByRole("button", { name: "Create package" }).dblclick();
  await expect(page.getByRole("heading", { level: 1, name: title })).toBeVisible();
  expect(new Set(posts).size).toBe(1);
  // (No `?mock=` here: that would start the mock server over.)
  await page.goto(`/admin/packages?q=${encodeURIComponent(title)}`);
  await expect(page.getByRole("table").getByRole("link", { name: title })).toHaveCount(1);
});

test("412 on a concurrent edit keeps my input and offers a diff", async ({ page }) => {
  const url = await createPackage(page, `Concurrent ${Date.now()}`);
  await page.getByLabel("Summary").fill("Mine");

  if (external) {
    const other = await page.context().newPage();
    await other.goto(url);
    await other.getByLabel("Summary").fill("Theirs");
    await other.getByRole("button", { name: "Save changes" }).click();
    await expect(other.getByText("Saved.")).toBeVisible();
    await other.close();
  } else {
    await page.evaluate((path) => {
      const id = path.split("/").pop() ?? "";
      const m = window.__adminMock;
      if (m) m.editElsewhere(m.db, id, { summary: "Theirs" });
    }, new URL(url).pathname);
  }

  await page.getByRole("button", { name: "Save changes" }).click();
  const panel = page.getByRole("alert").filter({ hasText: "Changed by someone else" });
  await expect(panel).toBeVisible();
  await expect(page.getByLabel("Summary")).toHaveValue("Mine");
  await expect(panel.getByRole("row", { name: /Summary/ })).toContainText("Theirs");
  await expect(panel.getByRole("row", { name: /Summary/ })).toContainText("Mine");
  await panel.getByRole("button", { name: "Keep my changes (then save)" }).click();
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.getByText("Saved.")).toBeVisible();
  await page.reload();
  await expect(page.getByLabel("Summary")).toHaveValue("Mine");
});

const BOUNDARIES: [string, number][] = [
  ["Title (required)", 200],
  ["Summary", 500],
  ["Developer", 200],
  ["Publisher", 200],
];

test("every field boundary: empty, max, max+1, unicode, RTL and emoji", async ({ page }) => {
  await createPackage(page, `Boundaries ${Date.now()}`);
  const save = page.getByRole("button", { name: "Save changes" });
  for (const [label, max] of BOUNDARIES) {
    const input = page.getByLabel(label);
    // 0 characters: allowed for optional fields, refused for the title.
    await input.fill("");
    await save.click();
    if (label.includes("required")) await expect(input).toHaveAttribute("aria-invalid", "true");
    else await expect(input).not.toHaveAttribute("aria-invalid");
    // max, with RTL text and emoji counted as one character each.
    const value = `${"ع".repeat(max - 2)}🎮é`;
    await input.fill(value);
    await save.click();
    await expect(page.getByText("Saved.")).toBeVisible();
    await expect(input).not.toHaveAttribute("aria-invalid");
    // max + 1.
    await input.fill(`${value}🚀`);
    await save.click();
    await expect(input).toHaveAttribute("aria-invalid", "true");
    await expect(page.getByText(`At most ${max} characters (now ${max + 1}).`)).toBeVisible();
    await input.fill(value);
  }
  await page.reload();
  await expect(page.getByLabel("Summary")).toHaveValue(`${"ع".repeat(498)}🎮é`);
});

test("keyboard only: filter the list and open a package", async ({ page }) => {
  const title = external ? `Hollow Harbor ${Date.now()}` : "Hollow Harbor";
  if (external) await createPackage(page, title);
  await page.goto(`/admin/packages${mock}`);
  await expect(page.getByRole("table")).toBeVisible();
  await page.getByLabel("Search title or slug").focus();
  await page.keyboard.type("harbor");
  await page.keyboard.press("Enter");
  const link = page.getByRole("table").getByRole("link", { name: title });
  await expect(link).toBeVisible();
  await link.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("heading", { level: 1, name: title })).toBeVisible();
});
