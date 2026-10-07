// INT-04: the same history UI checks run in mock and real-API mode.

import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";
import type { Schemas } from "@vgames/api-client";
import { realApi, signIn } from "./helpers";

const historyUrl = "**/v1/admin/packages/*/compat/linux*";
const revision = (number: number): Schemas["SignedCompatProfile"] => ({
  target: "linux",
  revision: number,
  status: "verified",
  document: "e30=",
  signature: {
    format: "vgames.sig/1",
    alg: "ed25519",
    context: "vgames/compat/v1",
    key_id: "a".repeat(32),
    payload_blake3: "b".repeat(64),
    signature: "AA==",
  },
  created_at: "2026-10-06T10:00:00Z",
});

async function editor(page: Page, beforeHistory?: () => Promise<void>) {
  await signIn(page);
  const title = `History fixture ${crypto.randomUUID()}`;
  await page.goto(`/admin/packages/new${realApi ? "" : "?mock=admin"}`);
  await page.getByLabel("Title (required)").fill(title);
  await page.getByRole("button", { name: "Create package" }).click();
  await expect(page.getByRole("heading", { level: 1, name: title })).toBeVisible();
  await beforeHistory?.();
  await page
    .getByRole("navigation", { name: "Package sections" })
    .getByRole("link", { name: "Compatibility" })
    .click();
  return page
    .getByRole("region", { name: "Linux (Proton)" })
    .getByRole("region", { name: "Revision history" });
}

async function scan(page: Page) {
  const results = await new AxeBuilder({ page }).analyze();
  expect(
    results.violations.filter((v) => v.impact === "serious" || v.impact === "critical"),
  ).toEqual([]);
}

test("revision history retains earlier pages and is accessible", async ({ page }) => {
  const cursors: (string | null)[] = [];
  await page.route(historyUrl, async (route) => {
    const cursor = new URL(route.request().url()).searchParams.get("cursor");
    cursors.push(cursor);
    await route.fulfill({
      json: cursor
        ? { items: [revision(1)] }
        : {
            items: [revision(3), revision(2)],
            next_cursor: "opaque-history-cursor",
          },
    });
  });
  const history = await editor(page, async () => {
    if (realApi) return;
    await page.evaluate(
      (items) => {
        if (!window.__adminMock) throw new Error("Mock server missing");
        window.__adminMock.db.compatHistoryFixture = {
          pages: {
            first: { items: items.slice(0, 2), next_cursor: "opaque-history-cursor" },
            "opaque-history-cursor": { items: items.slice(2) },
          },
          cursors: [],
        };
      },
      [revision(3), revision(2), revision(1)],
    );
  });
  await expect(history.getByText(/Revision 3:/)).toBeVisible();
  await expect(history.getByText("a".repeat(32))).toHaveCount(2);
  await scan(page);
  const more = history.getByRole("button", { name: "Load earlier revisions" });
  await more.focus();
  await page.keyboard.press("Enter");
  await expect(history.getByText(/Revision 1:/)).toBeVisible();
  await expect(history.getByRole("listitem")).toHaveCount(3);
  await expect(more).toHaveCount(0);
  const observed = realApi
    ? cursors
    : await page.evaluate(() => window.__adminMock?.db.compatHistoryFixture?.cursors);
  expect(observed).toEqual([null, "opaque-history-cursor"]);
  await scan(page);
});

test("empty revision history is accessible and offers no paging action", async ({ page }) => {
  await page.route(historyUrl, (route) => route.fulfill({ json: { items: [] } }));
  const history = await editor(page);
  await expect(history.getByText("No revisions yet.")).toBeVisible();
  await expect(history.getByRole("button")).toHaveCount(0);
  await scan(page);
});

test("a forbidden history explains the refusal without a retry action", async ({ page }) => {
  await page.route(historyUrl, (route) =>
    route.fulfill({
      status: 403,
      contentType: "application/problem+json",
      json: { type: "about:blank", title: "Not allowed", status: 403, code: "forbidden" },
    }),
  );
  const history = await editor(page, async () => {
    if (realApi) return;
    await page.evaluate(() => {
      if (!window.__adminMock) throw new Error("Mock server missing");
      window.__adminMock.db.compatHistoryFixture = { pages: {}, cursors: [], forbidden: true };
    });
  });
  await expect(history.getByRole("alert")).toContainText("Not allowed");
  await expect(history.getByRole("button")).toHaveCount(0);
  await scan(page);
});
