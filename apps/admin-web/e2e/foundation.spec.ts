import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";

test("signed-out users land on sign-in and can start Discord login", async ({ page }) => {
  await page.goto("/admin/users?mock=anon");
  await expect(page).toHaveURL(/\/admin\/login\?return_to=%2Fadmin%2Fusers/);
  const request = page.waitForRequest((r) => r.url().endsWith("/v1/auth/discord/start"));
  await page.route("https://discord.example/**", (route) => route.fulfill({ body: "discord" }));
  await page.getByRole("button", { name: "Sign in with Discord" }).click();
  expect((await request).postDataJSON()).toEqual({ client: "web", return_to: "/admin/users" });
  await expect(page).toHaveURL(/discord\.example/);
});

test("admins see the navigation; every page is reachable by keyboard", async ({ page }) => {
  await page.goto("/admin/?mock=admin");
  await expect(page.getByRole("heading", { name: "Packages" })).toBeVisible();
  const nav = page.getByRole("navigation", { name: "Admin" });
  for (const name of ["Users", "Allowlist", "Settings", "Trust", "Jobs", "Audit log", "Packages"]) {
    await nav.getByRole("link", { name }).focus();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("heading", { level: 1, name })).toBeVisible();
  }
});

test("no animations or transitions anywhere", async ({ page }) => {
  await page.goto("/admin/?mock=admin");
  await expect(page.getByRole("heading", { name: "Packages" })).toBeVisible();
  const moving = await page.evaluate(
    () =>
      Array.from(document.querySelectorAll("*")).filter((el) => {
        const s = getComputedStyle(el);
        return (
          s.animationName !== "none" ||
          s.transitionDuration.split(",").some((d) => Number.parseFloat(d) > 0)
        );
      }).length,
  );
  expect(moving).toBe(0);
});

for (const [name, path] of [
  ["login", "/admin/login?mock=anon"],
  ["shell", "/admin/packages?mock=admin"],
] as const) {
  test(`no serious accessibility violations: ${name}`, async ({ page }) => {
    await page.goto(path);
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
    const results = await new AxeBuilder({ page }).analyze();
    const serious = results.violations.filter(
      (v) => v.impact === "serious" || v.impact === "critical",
    );
    expect(
      serious.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`),
    ).toEqual([]);
  });
}
