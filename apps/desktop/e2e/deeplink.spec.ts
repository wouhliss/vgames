import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import { emit, open } from "./helpers";

for (const [reason, message] of [
  ["rate_limited", "Wait a moment before opening another game link."],
  ["not_installed", "Install this game before opening its launch link."],
  ["not_verified", "Verify this game's files before opening its launch link."],
  [
    "unknown_package",
    "This game link is unavailable on the selected server. Check that the correct server is selected.",
  ],
] as const) {
  test(`launch-link ${reason} is announced, axe clean and dismissible by keyboard`, async ({
    page,
  }) => {
    await open(page, "ready");
    await expect(page.getByRole("heading", { level: 1, name: "Library" })).toBeVisible();
    await emit(page, "deeplink-refused", { reason });
    await expect(page.getByText(message, { exact: true })).toBeVisible();
    const results = await new AxeBuilder({ page }).analyze();
    expect(results.violations).toEqual([]);
    const dismiss = page.getByRole("button", { name: "Dismiss notification" });
    await dismiss.focus();
    await page.keyboard.press("Enter");
    await expect(page.getByText(message, { exact: true })).toHaveCount(0);
  });
}
