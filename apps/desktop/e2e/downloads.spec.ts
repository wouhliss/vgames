// A3-T06 acceptance in the browser: the queue with live progress, keyboard-only and controller-only
// use, and accessibility. The `ready` preset has one job in every state (src/mocks/downloads.ts).
import { expect, type Locator, type Page, test } from "@playwright/test";
import { axeScan, commandCalls, emit, open } from "./helpers";

const pad = (page: Page, action: string) =>
  emit(page, "ui-nav", { action, controller: "xinput", repeat: false });

async function toDownloads(page: Page): Promise<void> {
  await open(page, "ready");
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Downloads" })
    .click();
  await expect(page.getByRole("heading", { level: 1, name: "Downloads" })).toBeVisible();
  await expect(page.getByRole("region", { name: "Up next" })).toBeVisible();
}

/** Titles of the queue's jobs, in order, as the mock backend holds them. */
async function jobTitles(page: Page): Promise<string[]> {
  return page.evaluate(
    () => window.__vgamesMock?.backend.state.downloads.map((j) => j.title) ?? [],
  );
}

const row = (page: Page, title: string): Locator =>
  page.getByRole("listitem").filter({ has: page.getByRole("heading", { level: 3, name: title }) });

const upNext = (page: Page) =>
  page.getByRole("region", { name: "Up next" }).getByRole("heading", { level: 3 });

test("has no serious accessibility violations (queue, menu, cancel dialog)", async ({ page }) => {
  await toDownloads(page);
  const scan = async () => {
    const results = await axeScan(page);
    const serious = results.violations.filter(
      (v) => v.impact === "serious" || v.impact === "critical",
    );
    expect(serious, JSON.stringify(serious, null, 2)).toEqual([]);
  };
  await scan();
  const [running] = await jobTitles(page);
  await page.getByRole("button", { name: `Cancel ${running}` }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await scan();
});

test("shows live progress of the running job", async ({ page }) => {
  await toDownloads(page);
  const [running] = await jobTitles(page);
  if (!running) throw new Error("no running job");
  const item = row(page, running);
  // The simulator reports speed, time left and connections within a second.
  await expect(item).toContainText(/Downloading · .+ of .+ · .+\/s · .+ left · 12 connections/);
  const before = Number(await item.getByRole("progressbar").getAttribute("aria-valuenow"));
  await expect
    .poll(async () => Number(await item.getByRole("progressbar").getAttribute("aria-valuenow")))
    .toBeGreaterThan(before);
});

test("works with the keyboard only: reorder, pause, cancel", async ({ page }) => {
  await toDownloads(page);
  const titles = await jobTitles(page);
  // Jobs 0–3: running, the first waiting job, another, and one paused with 2 GB downloaded.
  const [running, first, , partial] = titles;
  if (!running || !first || !partial) throw new Error("fixture");

  // Alt+Down moves the first waiting job down; focus stays on it.
  const more = row(page, first).getByRole("button", { name: `More actions for ${first}` });
  await more.focus();
  await page.keyboard.press("Alt+ArrowDown");
  await expect(upNext(page).nth(1)).toHaveText(first);
  await expect(more).toBeFocused();
  await expect(page.getByText(`${first} moved to position 2`)).toBeAttached();

  // Pause the running job from the keyboard.
  const pause = row(page, running).getByRole("button", { name: `Pause ${running}` });
  await pause.focus();
  await page.keyboard.press("Enter");
  await expect(row(page, running).getByRole("button", { name: `Resume ${running}` })).toBeVisible();
  expect(await commandCalls(page, "download_pause")).toHaveLength(1);

  // Cancel a partly downloaded job and delete its files.
  const cancel = row(page, partial).getByRole("button", { name: `Cancel ${partial}` });
  await cancel.focus();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("radio", { name: /Keep the downloaded files/ })).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(dialog.getByRole("radio", { name: /Delete the downloaded files/ })).toHaveAttribute(
    "aria-checked",
    "true",
  );
  for (let i = 0; i < 4; i += 1) {
    if (
      await dialog
        .getByRole("button", { name: "Cancel download" })
        .evaluate((el) => el === document.activeElement)
    )
      break;
    await page.keyboard.press("Tab");
  }
  await page.keyboard.press("Enter");
  await expect(dialog).toHaveCount(0);
  const calls = (await commandCalls(page, "download_cancel")) as { keepPartial: boolean }[];
  expect(calls[0]?.keepPartial).toBe(false);
  await expect(row(page, partial)).toHaveCount(0);
});

test("works with a controller only: move between jobs, open the menu, resume", async ({ page }) => {
  await toDownloads(page);
  const paused = await page.evaluate(
    () =>
      window.__vgamesMock?.backend.state.downloads.find(
        (j) => j.state.kind === "paused" && j.state.reason.kind === "user",
      )?.title ?? "",
  );
  const resume = row(page, paused).getByRole("button", { name: `Resume ${paused}` });
  await resume.focus();
  // Right reaches the row's other actions, left comes back.
  await pad(page, "right");
  await expect(resume).not.toBeFocused();
  await pad(page, "left");
  await expect(resume).toBeFocused();
  // A resumes it.
  await pad(page, "accept");
  await expect(row(page, paused)).toContainText("Waiting for its turn");
  expect(await commandCalls(page, "download_resume")).toHaveLength(1);
  // The menu opens with A on "More actions" and closes with B, focus back on the button.
  const more = row(page, paused).getByRole("button", { name: `More actions for ${paused}` });
  await more.focus();
  await pad(page, "accept");
  await expect(page.getByRole("menu")).toBeVisible();
  await pad(page, "back");
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(more).toBeFocused();
});

test("explains every waiting and failed state", async ({ page }) => {
  await toDownloads(page);
  await expect(
    page.getByText(/^Not enough space on \/home\/sam\/Games: .* needed, .* free\./),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "Open Storage settings" })).toBeVisible();
  await expect(page.getByText("The server has a damaged file")).toBeVisible();
  const security = page
    .getByRole("alert")
    .filter({ hasText: "Stopped to keep your computer safe" });
  await expect(security).toBeVisible();
  const securityRow = page.getByRole("listitem").filter({ has: security });
  await expect(securityRow.getByRole("button", { name: /again/ })).toHaveCount(0);
  await expect(page.getByRole("region", { name: "Completed" })).toContainText(
    "Failed: Permission denied",
  );
});
