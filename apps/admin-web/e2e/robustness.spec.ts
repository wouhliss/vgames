// A3-T18 in the browser: deep links to every page and entity, back/forward, keyboard-only use of
// forms and tables with a visible focus ring, accessibility in failure states, and a session that
// ends in the middle of a form. Runs against the MSW mock build by default and, except for the
// tests that inject faults, against the real stack nightly (ADMIN_E2E_BASE_URL).
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";

const external = Boolean(process.env.ADMIN_E2E_BASE_URL);
const mock = external ? "" : "?mock=admin";

async function serious(page: Page) {
  const results = await new AxeBuilder({ page }).analyze();
  return results.violations
    .filter((v) => v.impact === "serious" || v.impact === "critical")
    .map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
}

/** A fresh package made through the UI; returns its editor path (`/admin/packages/<id>`). */
async function createPackage(page: Page, title: string): Promise<string> {
  await page.goto(`/admin/packages/new${mock}`);
  await page.getByLabel("Title (required)").fill(title);
  await page.getByRole("button", { name: "Create package" }).click();
  await expect(page.getByRole("heading", { level: 1, name: title })).toBeVisible();
  return new URL(page.url()).pathname;
}

/**
 * The focused element shows a focus ring (`:focus-visible` outline or a box shadow). Polled: leaving
 * a date input's last segment, Chromium settles focus on the next element a moment after the key.
 */
async function expectFocusRing(page: Page) {
  await expect
    .poll(() =>
      page.evaluate(() => {
        const el = document.activeElement;
        if (!el || el === document.body) return "nothing focused";
        const s = getComputedStyle(el);
        const outline = s.outlineStyle !== "none" && Number.parseFloat(s.outlineWidth) > 0;
        return outline || s.boxShadow !== "none" ? "ok" : `no ring on ${el.outerHTML.slice(0, 80)}`;
      }),
    )
    .toBe("ok");
}

/** Presses Tab until the focused element matches, checking the focus ring on the way. */
async function tabTo(page: Page, matches: (el: { label: string; tag: string }) => boolean) {
  for (let i = 0; i < 60; i++) {
    await page.keyboard.press("Tab");
    await expectFocusRing(page);
    const el = await page.evaluate(() => {
      const a = document.activeElement as HTMLElement | null;
      const labelled = a?.id ? document.querySelector(`label[for="${a.id}"]`)?.textContent : null;
      return {
        label: (labelled ?? a?.getAttribute("aria-label") ?? a?.textContent ?? "").trim(),
        tag: a?.tagName ?? "",
      };
    });
    if (matches(el)) return;
  }
  throw new Error("not reachable with Tab");
}

test("every package section and server page opens from a deep link", async ({ page }) => {
  const title = `Deep link ${Date.now()}`;
  const editor = await createPackage(page, title);
  for (const [suffix, tab] of [
    ["", "Details"],
    ["/metadata", "Metadata"],
    ["/images", "Images"],
    ["/versions", "Versions"],
    ["/compatibility", "Compatibility"],
  ] as const) {
    await page.goto(`${editor}${suffix}`);
    await expect(page.getByRole("heading", { level: 1, name: title })).toBeVisible();
    const sections = page.getByRole("navigation", { name: "Package sections" });
    await expect(sections.getByRole("link", { name: tab })).toHaveAttribute("aria-current", "page");
  }
  await page.goto(`${editor}/versions/new`);
  await expect(page.getByRole("heading", { name: /upload/i }).first()).toBeVisible();

  // Filters live in the URL: a shared link shows the same filtered view.
  await page.goto(`/admin/packages?q=${encodeURIComponent(title)}`);
  await expect(page.getByLabel(/Search/)).toHaveValue(title);
  await expect(page.getByRole("table").getByRole("link", { name: title })).toHaveCount(1);
  await page.goto("/admin/jobs?state=failed");
  await expect(page.getByLabel("State")).toHaveValue("failed");
  await page.goto("/admin/users?role=admin");
  await expect(page.getByLabel("Role")).toHaveValue("admin");
  await page.goto("/admin/no-such-page");
  await expect(page.getByRole("heading", { level: 1, name: "Not found" })).toBeVisible();
  await page.goto("/admin/packages/01920000-0000-7000-8000-00000000ffff");
  await expect(page.getByRole("heading", { level: 1, name: "Not found" })).toBeVisible();
});

test("back and forward walk the history, filters included", async ({ page }) => {
  const title = `History ${Date.now()}`;
  const editor = await createPackage(page, title);
  await page.goto(`/admin/packages?q=${encodeURIComponent(title)}`);
  await page.getByRole("table").getByRole("link", { name: title }).click();
  const sections = page.getByRole("navigation", { name: "Package sections" });
  await sections.getByRole("link", { name: "Versions" }).click();
  await sections.getByRole("link", { name: "Compatibility" }).click();
  await expect(page).toHaveURL(new RegExp(`${editor}/compatibility$`));

  await page.goBack();
  await expect(sections.getByRole("link", { name: "Versions" })).toHaveAttribute(
    "aria-current",
    "page",
  );
  await page.goBack();
  await expect(sections.getByRole("link", { name: "Details" })).toHaveAttribute(
    "aria-current",
    "page",
  );
  await page.goBack();
  await expect(page.getByRole("heading", { level: 1, name: "Packages" })).toBeVisible();
  await expect(page.getByLabel(/Search/)).toHaveValue(title);
  await page.goForward();
  await expect(page.getByRole("heading", { level: 1, name: title })).toBeVisible();
});

test("keyboard only: create a package, edit it and save", async ({ page }) => {
  const title = `Keyboard ${Date.now()}`;
  await page.goto(`/admin/packages/new${mock}`);
  // The title field has focus when the form opens.
  await expect(page.getByLabel("Title (required)")).toBeFocused();
  await page.keyboard.type(title);
  await page.keyboard.press("Enter");
  await expect(page.getByRole("heading", { level: 1, name: title })).toBeVisible();

  await tabTo(page, (el) => el.label.startsWith("Summary"));
  await page.keyboard.type("Typed without a mouse.");
  await tabTo(page, (el) => el.tag === "BUTTON" && el.label === "Save changes");
  await page.keyboard.press("Enter");
  await expect(page.getByText("Saved.")).toBeVisible();
  await page.reload();
  await expect(page.getByLabel("Summary")).toHaveValue("Typed without a mouse.");
});

test("keyboard only: filter a table and open a row", async ({ page }) => {
  const title = `Row ${Date.now()}`;
  await createPackage(page, title);
  await page.goto("/admin/packages");
  await expect(page.getByRole("table")).toBeVisible();
  await tabTo(page, (el) => el.label.startsWith("Search"));
  await page.keyboard.type(title);
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/q=/);
  await tabTo(page, (el) => el.tag === "A" && el.label === title);
  await page.keyboard.press("Enter");
  await expect(page.getByRole("heading", { level: 1, name: title })).toBeVisible();

  await page.goto("/admin/jobs");
  await expect(page.getByRole("table")).toBeVisible();
  await tabTo(page, (el) => el.label === "State");
  await page.keyboard.press("ArrowDown");
  await tabTo(page, (el) => el.tag === "BUTTON" && el.label === "Apply");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/state=/);
});

test.describe("failure states (mock server)", () => {
  test.skip(external, "injects faults into the mock server");

  async function fault(page: Page, api: number | "offline" | undefined) {
    await page.waitForFunction(() => window.__adminMock !== undefined);
    await page.evaluate((f) => {
      const m = window.__adminMock;
      if (m) m.db.faults.api = f;
    }, api);
  }

  for (const [state, says] of [
    [500, "Server error"],
    [429, "Too many requests"],
    [403, "Not allowed"],
    ["offline", "Can't reach the server"],
  ] as const) {
    test(`every page says so and stays accessible: ${state}`, async ({ page }) => {
      test.setTimeout(90_000);
      await page.goto("/admin/packages?mock=owner");
      await expect(page.getByRole("heading", { level: 1, name: "Packages" })).toBeVisible();
      await fault(page, state);
      // The packages list was loaded (and cached) before the fault; it is checked last.
      for (const path of ["users", "allowlist", "settings", "trust", "jobs", "audit"]) {
        // In-app navigation: a reload would start the mock server over.
        await page.evaluate((p) => {
          window.history.pushState(null, "", `/admin/${p}`);
          window.dispatchEvent(new PopStateEvent("popstate"));
        }, path);
        await expect(page.getByText(says, { exact: true }).first()).toBeVisible({
          timeout: 10_000,
        });
        expect(await serious(page), `${state} /admin/${path}`).toEqual([]);
      }
      // A new filter is a new request (the unfiltered list is still cached from before the fault).
      await page.evaluate(() => {
        window.history.pushState(null, "", "/admin/packages?q=anything");
        window.dispatchEvent(new PopStateEvent("popstate"));
      });
      await expect(page.getByText(says, { exact: true }).first()).toBeVisible({ timeout: 10_000 });
      expect(await serious(page), `${state} /admin/packages`).toEqual([]);
    });
  }

  test("a session that ends mid-form keeps the input through sign-in", async ({ page }) => {
    const editor = "/admin/packages/01920000-0000-7000-8000-0000000b0001";
    const dialogs: string[] = [];
    page.on("dialog", (d) => {
      dialogs.push(d.message());
      void d.dismiss();
    });
    await page.goto(`${editor}?mock=admin`);
    const summary = page.getByLabel("Summary");
    await summary.fill("Written as the session ran out");
    await page.waitForFunction(() => window.__adminMock !== undefined);
    await page.evaluate(() => {
      const m = window.__adminMock;
      if (m) m.db.role = null;
    });
    await page.getByRole("button", { name: "Save changes" }).click();
    await expect(page).toHaveURL(/\/admin\/login\?return_to=%2Fadmin%2Fpackages%2F/);
    await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();

    // Discord sends the browser back: a full page load of the page it left.
    await page.goto(editor);
    await expect(page.getByLabel("Summary")).toHaveValue("Written as the session ran out");
    await expect(
      page.getByText("Restored the changes you hadn't saved before signing in again", {
        exact: false,
      }),
    ).toBeVisible();
    await page.getByRole("button", { name: "Save changes" }).click();
    await expect(page.getByText("Saved.")).toBeVisible();
    expect(dialogs).toEqual([]);
  });
});
