// A3-T17 acceptance: the authorization matrix, admin vs owner, for every page and action. Owner-only
// controls are hidden from admins; every page loads for both; axe passes on each. Mock mode (the
// real stack runs the same matrix with two signed-in accounts in the nightly workflow).
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";

test.skip(Boolean(process.env.ADMIN_E2E_BASE_URL), "signs in through the mock server");

type Role = "admin" | "owner";

interface Check {
  page: string;
  heading: RegExp;
  /** Controls only owners get. */
  ownerOnly: { name: string; find: (page: Page) => ReturnType<Page["getByRole"]> }[];
  /** Controls both roles get. */
  both: { name: string; find: (page: Page) => ReturnType<Page["getByRole"]> }[];
}

const MATRIX: Check[] = [
  {
    page: "/admin/packages",
    heading: /^Packages$/,
    ownerOnly: [],
    both: [
      { name: "create package", find: (p) => p.getByRole("link", { name: "Create package" }) },
    ],
  },
  {
    page: "/admin/packages/01920000-0000-7000-8000-0000000b0001/versions",
    heading: /Hollow Harbor/,
    ownerOnly: [],
    both: [
      { name: "publish", find: (p) => p.getByRole("button", { name: "Publish…" }) },
      { name: "yank", find: (p) => p.getByRole("button", { name: "Yank…" }) },
    ],
  },
  {
    page: "/admin/users",
    heading: /^Users$/,
    ownerOnly: [
      { name: "role change", find: (p) => p.getByRole("combobox", { name: "Role of sam" }) },
    ],
    both: [
      { name: "disable a user", find: (p) => p.getByRole("button", { name: "Disable sam…" }) },
    ],
  },
  {
    page: "/admin/allowlist",
    heading: /^Allowlist$/,
    ownerOnly: [],
    both: [{ name: "add", find: (p) => p.getByRole("button", { name: "Add" }) }],
  },
  {
    page: "/admin/settings",
    heading: /Server settings/,
    ownerOnly: [
      { name: "save settings", find: (p) => p.getByRole("button", { name: "Save settings" }) },
    ],
    both: [],
  },
  {
    page: "/admin/trust",
    heading: /^Trust$/,
    ownerOnly: [
      { name: "upload bundle", find: (p) => p.getByRole("button", { name: "Upload bundle" }) },
    ],
    both: [],
  },
  {
    page: "/admin/jobs",
    heading: /^Jobs$/,
    ownerOnly: [],
    both: [{ name: "retry", find: (p) => p.getByRole("button", { name: /^Retry/ }).first() }],
  },
  {
    page: "/admin/audit",
    heading: /^Audit log$/,
    ownerOnly: [],
    both: [{ name: "filter", find: (p) => p.getByRole("button", { name: "Apply" }) }],
  },
];

for (const role of ["admin", "owner"] as Role[]) {
  test(`authorization matrix: ${role}`, async ({ page }) => {
    test.setTimeout(90_000);
    await page.goto(`/admin/packages?mock=${role}`);
    await expect(page.getByRole("heading", { level: 1, name: "Packages" })).toBeVisible();
    for (const check of MATRIX) {
      await page.goto(check.page);
      await expect(page.getByRole("heading", { level: 1, name: check.heading })).toBeVisible();
      await expect(page.getByRole("status").filter({ hasText: /^Loading/ })).toHaveCount(0);
      for (const control of check.both) {
        await expect(control.find(page), `${role} ${check.page}: ${control.name}`).toBeVisible();
      }
      for (const control of check.ownerOnly) {
        await expect(control.find(page), `${role} ${check.page}: ${control.name}`).toHaveCount(
          role === "owner" ? 1 : 0,
        );
      }
      const results = await new AxeBuilder({ page }).analyze();
      const serious = results.violations.filter(
        (v) => v.impact === "serious" || v.impact === "critical",
      );
      expect(serious, `${role} ${check.page}: ${JSON.stringify(serious, null, 2)}`).toEqual([]);
    }
  });
}

test("an admin who forces an owner-only action gets a clear refusal (403)", async ({ page }) => {
  await page.goto("/admin/users?mock=admin");
  await expect(page.getByRole("table")).toBeVisible();
  // Admins can't disable admins: the button isn't there; the API also refuses if asked directly.
  await expect(page.getByRole("button", { name: "Disable olive…" })).toHaveCount(0);
  const status = await page.evaluate(async () => {
    const csrf = document.cookie.match(/__Host-vgames_csrf=([^;]+)/)?.[1] ?? "";
    const res = await fetch("/v1/admin/users/01920000-0000-7000-8000-00000000a001", {
      method: "PATCH",
      headers: { "Content-Type": "application/merge-patch+json", "X-CSRF-Token": csrf },
      body: JSON.stringify({ role: "user" }),
    });
    return res.status;
  });
  expect(status).toBe(403);
});
