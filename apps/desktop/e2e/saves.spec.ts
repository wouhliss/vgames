// PLAY-06 acceptance in the browser: the cloud-save conflict dialog and Settings → Cloud saves pass
// axe and work with the keyboard only and with a controller only. The `ready` preset's first
// playable game gets a conflict (and the launch refusal the Rust hook would return) and a history.
import { expect, type Page, test } from "@playwright/test";
import { axeViolations, commandCalls, emit, open } from "./helpers";

const pad = (page: Page, action: string) =>
  emit(page, "ui-nav", { action, controller: "xinput", repeat: false });

interface Game {
  title: string;
  package: { server_id: string; package_id: string };
  conflict: Record<string, unknown>;
}

/** Gives the first playable game a conflict and a save history; returns it. */
async function withConflict(page: Page): Promise<Game> {
  await open(page, "ready");
  await expect(page.getByRole("heading", { level: 1, name: "Library" })).toBeVisible();
  await page.waitForFunction(() => window.__vgamesMock !== undefined);
  return page.evaluate(() => {
    const mock = window.__vgamesMock;
    if (!mock) throw new Error("mock mode");
    const state = mock.backend.state;
    const online = new Set(state.libraries.filter((l) => l.online).map((l) => l.id));
    const game = state.installs.find(
      (i) => i.state === "installed" && !i.running && online.has(i.library_id),
    );
    if (!game) throw new Error("no playable game in the preset");
    game.cloud_saves = "conflict";
    const id = "c0ffee00-0000-7000-8000-0000000000e2";
    const side = (kind: "cloud" | "backup", files: number, device: string | null) => ({
      source:
        kind === "cloud"
          ? { kind, snapshot_id: "5a5e0000-0000-7000-8000-0000000000e2" }
          : { kind, backup_id: "bacc0000-0000-7000-8000-0000000000e2" },
      created_at: "2026-10-24T20:10:00Z",
      device_name: device,
      file_count: files,
      total_bytes: files * 1_000_000,
    });
    const conflict = {
      conflict_id: id,
      package: game.package,
      cloud: side("cloud", 3, "Living room PC"),
      device: side("backup", 4, null),
      created_at: "2026-10-24T20:10:00Z",
    };
    state.saveConflicts[id] = conflict as (typeof state.saveConflicts)[string];
    state.launchErrors[game.package.package_id] = { kind: "save_conflict", conflict_id: id };
    state.saveHistories[game.package.package_id] = {
      package: game.package,
      head_snapshot_id: "5a5e0000-0000-7000-8000-0000000000e3",
      entries: [
        {
          ...side("cloud", 3, "Living room PC"),
          source: { kind: "cloud", snapshot_id: "5a5e0000-0000-7000-8000-0000000000e3" },
        },
        {
          ...side("cloud", 2, "Laptop"),
          source: { kind: "cloud", snapshot_id: "5a5e0000-0000-7000-8000-0000000000e4" },
        },
      ],
    } as (typeof state.saveHistories)[string];
    return { title: game.title, package: game.package, conflict };
  });
}

/** Moves focus with the controller until `name` is focused (bounded). */
async function padTo(page: Page, name: string, direction: string): Promise<void> {
  for (let i = 0; i < 8; i += 1) {
    const focused = await page.evaluate(() => document.activeElement?.textContent ?? "");
    if (focused.trim() === name) return;
    await pad(page, direction);
  }
  throw new Error(`could not reach ${name} with the controller`);
}

test("the conflict dialog opens from the refused launch, passes axe and keeps the cloud saves with the keyboard only", async ({
  page,
}) => {
  const game = await withConflict(page);
  await page.getByRole("button", { name: `Play ${game.title}` }).click();
  const resolve = page.getByRole("button", { name: "Choose saves" });
  await resolve.focus();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: `Choose which saves to keep for ${game.title}` });
  await expect(dialog.getByRole("region", { name: "In the cloud" })).toContainText(
    "Living room PC",
  );
  await expect(dialog.getByRole("region", { name: "On this device" })).toContainText("4 files");

  const violations = await axeViolations(page, '[role="dialog"]');
  const serious = violations.filter((v) => v.impact === "serious" || v.impact === "critical");
  expect(serious).toEqual([]);

  // Tab to the choice; nothing was sent before it.
  expect(await commandCalls(page, "save_conflict_resolve")).toEqual([]);
  for (let i = 0; i < 6; i += 1) {
    const focused = await page.evaluate(() => document.activeElement?.textContent ?? "");
    if (focused.trim() === "Keep cloud saves") break;
    await page.keyboard.press("Tab");
  }
  await page.keyboard.press("Enter");
  await expect(dialog).toBeHidden();
  expect(await commandCalls(page, "save_conflict_resolve")).toEqual([
    { conflictId: game.conflict.conflict_id, choice: "keep_cloud" },
  ]);
  // The game is not started for the player.
  expect(await commandCalls(page, "game_launch")).toHaveLength(1);
});

test("the conflict dialog works with a controller only, and B changes nothing", async ({
  page,
}) => {
  const game = await withConflict(page);
  await emit(page, "save-conflict", game.conflict);
  const dialog = page.getByRole("dialog", { name: `Choose which saves to keep for ${game.title}` });
  await expect(dialog.getByRole("region", { name: "In the cloud" })).toBeVisible();
  await pad(page, "back");
  await expect(dialog).toBeHidden();
  expect(await commandCalls(page, "save_conflict_resolve")).toEqual([]);

  await emit(page, "save-conflict", game.conflict);
  await expect(dialog.getByRole("region", { name: "In the cloud" })).toBeVisible();
  await padTo(page, "Keep this device's saves", "right");
  await pad(page, "accept");
  await expect(dialog).toBeHidden();
  expect(await commandCalls(page, "save_conflict_resolve")).toEqual([
    { conflictId: game.conflict.conflict_id, choice: "keep_device" },
  ]);
});

test("Settings → Cloud saves restores an earlier save with the keyboard only, after confirming", async ({
  page,
}) => {
  const game = await withConflict(page);
  // Restoring waits for the conflict; resolve it first, as a player would.
  await page.evaluate(() => {
    const state = window.__vgamesMock?.backend.state;
    if (state) state.saveConflicts = {};
  });
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Settings" })
    .click();
  await page
    .getByRole("navigation", { name: "Settings sections" })
    .getByRole("link", { name: "Cloud saves" })
    .click();
  const section = page.getByRole("region", { name: "Cloud saves" });
  await expect(section).toBeVisible();
  const history = section.getByRole("button", { name: `Save history for ${game.title}` });
  await history.focus();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: `Save history for ${game.title}` });
  await expect(dialog.getByRole("listitem")).toHaveCount(2);
  const violations = await axeViolations(page, '[role="dialog"]');
  expect(violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);

  const restore = dialog.getByRole("button", { name: /^Restore the save from/ });
  await expect(restore).toHaveCount(1);
  await restore.focus();
  await page.keyboard.press("Enter");
  const confirm = page.getByRole("alertdialog", { name: "Restore this save?" });
  await expect(confirm).toContainText("backed up first");
  await confirm.getByRole("button", { name: "Restore" }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByText(`Saves for ${game.title} restored.`)).toBeVisible();
  expect(await commandCalls(page, "saves_restore")).toEqual([
    {
      pkg: game.package,
      source: { kind: "cloud", snapshot_id: "5a5e0000-0000-7000-8000-0000000000e4" },
    },
  ]);
});
