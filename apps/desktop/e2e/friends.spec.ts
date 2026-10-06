// A3-T09 in the browser: the friends, messages and requests screens, the add-friend and safety-number
// dialogs and the invite card pass axe; the invite card works from the keyboard and from a controller
// through to the install dialog.
import { expect, type Page, test } from "@playwright/test";
import { axeScan, commandCalls, emit, open } from "./helpers";

const pad = (page: Page, action: string) =>
  emit(page, "ui-nav", { action, controller: "xinput", repeat: false });

async function serious(page: Page, name: string) {
  const results = await axeScan(page);
  const found = results.violations.filter((v) => v.impact === "serious" || v.impact === "critical");
  expect(found, `${name}: ${JSON.stringify(found, null, 2)}`).toEqual([]);
}

async function toFriends(page: Page) {
  await open(page, "ready");
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Friends" })
    .click();
  await expect(page.getByRole("heading", { level: 1, name: "Friends" })).toBeVisible();
  await expect(page.getByText("Alex", { exact: true })).toBeVisible();
}

/** Sends an incoming invite for a game that isn't installed, the way the core does. */
async function receiveInvite(page: Page) {
  await page.waitForFunction(() => window.__vgamesMock !== undefined);
  const invite = await page.evaluate(() => {
    const mock = window.__vgamesMock;
    if (!mock) throw new Error("no mock");
    const state = mock.backend.state;
    const installed = new Set(state.installs.map((i) => i.package.package_id));
    const pkg = state.packages.find((p) => !installed.has(p.id));
    if (!pkg) throw new Error("no missing package");
    const invite = {
      id: "01920000-0000-7000-8000-00000000e0c1",
      direction: "incoming",
      from: {
        id: "01920000-0000-7000-8000-0000000u0001",
        username: "alex",
        display_name: "Alex",
        avatar_url: null,
      },
      to: {
        id: "01920000-0000-7000-8000-00000000a001",
        username: "sam",
        display_name: "Sam",
        avatar_url: null,
      },
      package: { id: pkg.id, title: pkg.title, cover_url: null },
      state: "pending",
      progress: null,
      message: "One more for the raid?",
      failure_reason: null,
      created_at: new Date().toISOString(),
      updated_at: new Date().toISOString(),
      expires_at: new Date(Date.now() + 600_000).toISOString(),
      has_join_secret: false,
    } as const;
    state.invites = [invite, ...state.invites];
    return invite;
  });
  await emit(page, "invite-received", invite);
  return invite;
}

test("friends screens and dialogs have no serious accessibility violations", async ({ page }) => {
  await toFriends(page);
  await serious(page, "friends");
  await page.getByRole("button", { name: "Add friend" }).first().click();
  await expect(page.getByRole("dialog", { name: "Add a friend" })).toBeVisible();
  await serious(page, "add friend");
  await page.keyboard.press("Escape");

  await page.getByRole("tab", { name: /Requests/ }).click();
  await expect(page.getByText("Requests for you")).toBeVisible();
  await serious(page, "requests");

  await page.getByRole("tab", { name: /Messages/ }).click();
  await page.getByRole("navigation", { name: "Conversations" }).getByRole("link").first().click();
  await expect(page.getByRole("log", { name: "Messages with Alex" })).toBeVisible();
  await serious(page, "messages");
  await page.getByRole("button", { name: "Safety number" }).click();
  await expect(page.getByTestId("safety-number")).toBeVisible();
  await serious(page, "safety number");
  await page.keyboard.press("Escape");

  await receiveInvite(page);
  await expect(page.getByRole("region", { name: "Alex invites you to play" })).toBeVisible();
  await serious(page, "invite card");
});

test("keyboard only: invite card → Accept → install dialog", async ({ page }) => {
  await open(page, "ready");
  await expect(page.getByRole("heading", { level: 1, name: "Library" })).toBeVisible();
  const invite = await receiveInvite(page);
  const card = page.getByRole("region", { name: "Alex invites you to play" });
  await expect(card).toContainText("One more for the raid?");
  const accept = card.getByRole("button", { name: "Accept" });
  await expect(async () => {
    await accept.focus();
    await expect(accept).toBeFocused({ timeout: 200 });
  }).toPass();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: `Install ${invite.package.title}` });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("button", { name: /Install/ }).last()).toBeVisible();
  expect(await commandCalls(page, "invite_accept")).toHaveLength(1);
  await serious(page, "install from invite");
});

test("controller only: invite card → Accept → install dialog, B closes it", async ({ page }) => {
  await open(page, "ready");
  await expect(page.getByRole("heading", { level: 1, name: "Library" })).toBeVisible();
  const invite = await receiveInvite(page);
  const accept = page
    .getByRole("region", { name: "Alex invites you to play" })
    .getByRole("button", {
      name: "Accept",
    });
  await expect(async () => {
    await accept.focus();
    await expect(accept).toBeFocused({ timeout: 200 });
  }).toPass();
  await pad(page, "accept");
  const dialog = page.getByRole("dialog", { name: `Install ${invite.package.title}` });
  await expect(dialog).toBeVisible();
  await pad(page, "back");
  await expect(dialog).toHaveCount(0);
});

test("keyboard only: add a friend with a code", async ({ page }) => {
  await toFriends(page);
  await page.getByRole("button", { name: "Add friend" }).first().focus();
  await page.keyboard.press("Enter");
  const field = page.getByRole("textbox", { name: "Friend code" });
  await expect(field).toBeFocused();
  await page.keyboard.type("7k2m-q9xd");
  await expect(page.getByText("Request sent to Ivo")).toBeVisible();
  await page.getByRole("tab", { name: /Requests/ }).click();
  await expect(page.getByRole("button", { name: "Cancel request: Ivo" })).toBeVisible();
});

test("keyboard only: send a message", async ({ page }) => {
  await toFriends(page);
  await page.getByRole("button", { name: "Message: Alex" }).focus();
  await page.keyboard.press("Enter");
  const box = page.getByRole("textbox", { name: "Message Alex" });
  await box.focus();
  await page.keyboard.type("On my way");
  await page.keyboard.press("Enter");
  const log = page.getByRole("log", { name: "Messages with Alex" });
  const item = log.getByRole("listitem").filter({ hasText: "On my way" });
  await expect(item).toContainText("Sent");
});

test("the sender sees the invitee's install progress", async ({ page }) => {
  await toFriends(page);
  const sent = page.getByRole("region", { name: "Invites you sent" });
  await expect(sent).toContainText("Bea is installing (42%)");
  await expect(sent.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "42");
});
