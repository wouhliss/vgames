// A3-T16 in the browser (mock server, real Web Workers): the upload wizard with a mid-way network cut
// and a page reload that resumes and completes; a wrong passphrase; a key not in the trust bundle; a
// very large folder stays responsive. Against the real stack (nightly), the same flow runs with
// pack-wasm and a real key file (not covered here).
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";

test.skip(Boolean(process.env.ADMIN_E2E_BASE_URL), "drives the mock server");

const HARBOR = "01920000-0000-7000-8000-0000000b0001";
const MiB = 1024 * 1024;
const PASSPHRASE = "correct horse battery staple";
const KEY = JSON.stringify({
  format: "vgames.key/mock",
  kind: "publisher",
  key_id: "5a1ddc0a5e2e4a3c9f1b7d2e8c4a6b10",
  label: "Adrian's laptop",
  passphrase: PASSPHRASE,
});

function makeFolder(files: [string, number][]): string {
  const root = join(mkdtempSync(join(tmpdir(), "vgames-upload-")), "MyGame");
  for (const [path, size] of files) {
    const full = join(root, path);
    mkdirSync(join(full, ".."), { recursive: true });
    writeFileSync(full, Buffer.alloc(size, path.length));
  }
  return root;
}

const GAME: [string, number][] = [
  ["bin/game.exe", 12 * MiB],
  ["data/level1.pak", 20 * MiB],
  ["data/level2.pak", 9 * MiB],
  ["readme.txt", 1000],
];

async function keyStep(page: Page, passphrase = PASSPHRASE, key = KEY) {
  await page.getByLabel("Publisher key file").setInputFiles({
    name: "adrian.vgkey",
    mimeType: "application/octet-stream",
    buffer: Buffer.from(key),
  });
  await page.getByLabel("Passphrase").fill(passphrase);
  await page.getByRole("button", { name: "Unlock key" }).click();
}

async function setFault(page: Page, down: boolean) {
  await page.evaluate((d) => {
    const m = window.__adminMock;
    if (m) m.db.faults.networkDown = d;
    if (!d) window.dispatchEvent(new Event("online"));
  }, down);
}

test("network cut and page reload: the upload resumes and completes", async ({ page }) => {
  const folder = makeFolder(GAME);
  await page.goto(`/admin/packages/${HARBOR}/versions/new?mock=admin`);
  await page.getByLabel("Version label").fill("2.0.0");
  await page.getByRole("button", { name: "Create version" }).click();
  await expect(page.getByRole("region", { name: "1. Version" })).toContainText("windows-x86_64");
  await page.getByLabel("Game folder").setInputFiles(folder);
  await expect(page.getByText(/4 files, .*, \d packs/)).toBeVisible();
  expect(
    await new AxeBuilder({ page })
      .analyze()
      .then((r) => r.violations.filter((v) => v.impact === "serious" || v.impact === "critical")),
  ).toEqual([]);
  await page.getByLabel("Executable (path in the folder)").fill("bin/game.exe");
  await keyStep(page, "wrong");
  await expect(page.getByText("The passphrase is wrong.")).toBeVisible();
  await keyStep(page);
  await expect(page.getByText(/Key ready/)).toBeVisible();

  // Cut the network once the first bytes are confirmed.
  await setFault(page, true);
  await page.getByRole("button", { name: "Start upload" }).click();
  await expect(page.getByText(/The connection dropped/)).toBeVisible({ timeout: 15_000 });
  await setFault(page, false);
  await expect(page.getByText("Uploading…")).toBeVisible({ timeout: 15_000 });

  // Reload mid-way: the progress is kept, the folder is picked again, the key unlocked again.
  await expect(page.getByRole("cell", { name: "Uploaded" }).first()).toBeVisible({
    timeout: 20_000,
  });
  page.once("dialog", (d) => void d.accept());
  await page.reload();
  await page.goto(`/admin/packages/${HARBOR}/versions`);
  await page.getByRole("link", { name: "Continue upload" }).first().click();
  await expect(page.getByText(/Pick the same folder again/)).toBeVisible();
  await page.getByLabel("Game folder").setInputFiles(folder);
  await expect(page.getByText(/4 files/)).toBeVisible();
  await keyStep(page);
  await page.getByRole("button", { name: "Continue upload" }).click();
  await expect(page.getByText("Verified. The version is ready to publish.")).toBeVisible({
    timeout: 60_000,
  });
  await page.getByRole("button", { name: "Publish 2.0.0" }).click();
  await expect(page.getByText(/Published 2\.0\.0/)).toBeVisible();
});

test("a key that isn't in the trust bundle is explained (422 publisher_key_untrusted)", async ({
  page,
}) => {
  const folder = makeFolder([["game.exe", 3 * MiB]]);
  await page.goto(`/admin/packages/${HARBOR}/versions/new?mock=admin`);
  await page.getByLabel("Version label").fill("2.1.0");
  await page.getByRole("button", { name: "Create version" }).click();
  await page.getByLabel("Game folder").setInputFiles(folder);
  await page.getByLabel("Executable (path in the folder)").fill("game.exe");
  await keyStep(page, PASSPHRASE, KEY.replace("5a1ddc0a5e2e4a3c9f1b7d2e8c4a6b10", "f".repeat(32)));
  await page.getByRole("button", { name: "Start upload" }).click();
  const alert = page.getByRole("alert").filter({ hasText: "The server refused the version" });
  await expect(alert).toContainText("isn't in the server's trust bundle", { timeout: 30_000 });
  await expect(alert.getByRole("button", { name: "Use another key file" })).toBeVisible();
});

test("a 100,000-file folder stays responsive (virtualized preview)", async ({ page }) => {
  test.setTimeout(240_000);
  const root = join(mkdtempSync(join(tmpdir(), "vgames-many-")), "Many");
  for (let d = 0; d < 100; d += 1) {
    mkdirSync(join(root, `d${d}`), { recursive: true });
    for (let f = 0; f < 1000; f += 1) writeFileSync(join(root, `d${d}`, `f${f}.txt`), "x");
  }
  await page.goto(`/admin/packages/${HARBOR}/versions/new?mock=admin`);
  await page.getByLabel("Version label").fill("big");
  await page.getByRole("button", { name: "Create version" }).click();
  await page.getByLabel("Game folder").setInputFiles(root);
  await expect(page.getByText(/100,000 files/)).toBeVisible({ timeout: 120_000 });
  const list = page.getByRole("region", { name: "Files to upload" });
  expect(await list.locator("li").count()).toBeLessThan(100);
  // The page answers input right away: typing lands within a frame or two.
  const started = Date.now();
  await page.getByLabel("Executable (path in the folder)").fill("d0/f0.txt");
  expect(Date.now() - started).toBeLessThan(1000);
  await list.focus();
  await page.keyboard.press("End");
  await expect(list.getByText("d99/f999.txt")).toBeVisible();
});
