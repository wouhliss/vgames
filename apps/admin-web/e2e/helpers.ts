import { expect, type Page } from "@playwright/test";

export const realApi = Boolean(process.env.ADMIN_E2E_BASE_URL);

// Exercise the web OAuth flow, including the callback cookie, against the debug-only
// fake provider. The synthetic account is the nightly stack's bootstrap owner.
export async function signIn(page: Page) {
  if (!realApi) return;
  await page.goto("/admin/login");
  await page.getByRole("button", { name: "Sign in with Discord" }).click();
  await expect(page).toHaveURL(/\/v1\/auth\/dev\/fake-discord\?state=/);
  const submit = new URL("/v1/auth/dev/fake-discord/submit", page.url());
  const state = new URL(page.url()).searchParams.get("state");
  if (!state) throw new Error("Fake Discord did not receive an OAuth state");
  submit.searchParams.set("state", state);
  submit.searchParams.set("id", "100000000000000001");
  submit.searchParams.set("name", "owner");
  await page.goto(submit.href);
  await expect(page).toHaveURL(/\/admin\//);
  await expect(page.getByRole("heading", { name: "Packages", exact: true })).toBeVisible();
}
