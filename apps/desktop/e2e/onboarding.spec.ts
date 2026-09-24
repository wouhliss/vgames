// A3-T03 acceptance: one test per onboarding edge case, against mockIPC fixtures (src/mocks).
import { expect, test } from "@playwright/test";
import {
  commandCalls,
  emit,
  FINGERPRINT,
  OTHER_FINGERPRINT,
  open,
  reachSignIn,
  submitAddress,
  VALID_CODE,
} from "./helpers";

test.describe("server address", () => {
  test.beforeEach(async ({ page }) => {
    await open(page, "fresh");
    await expect(page.getByRole("heading", { name: "Welcome to vgames" })).toBeVisible();
  });

  const cases: [string, string, RegExp][] = [
    ["empty", "", /Enter the server's address/],
    ["malformed URL", "not a url", /isn't a valid address/],
    ["unsupported scheme", "ftp://games.example.org", /isn't a valid address/],
    ["http:// refused", "http://games.example.org", /unencrypted connection/],
    ["unreachable", "https://unreachable.example", /server didn't answer/],
    ["timeout", "https://slow.example", /took too long/],
    ["TLS error", "https://bad-cert.example", /security certificate isn't valid/],
    ["not a vgames server", "https://not-vgames.example", /isn't a vgames server/],
  ];
  for (const [name, address, message] of cases) {
    test(name, async ({ page }) => {
      await submitAddress(page, address);
      const field = page.getByRole("textbox", { name: /Server address/ });
      await expect(field).toHaveAttribute("aria-invalid", "true");
      await expect(field).toHaveAccessibleDescription(message);
      await expect(field).toBeFocused();
      expect(await commandCalls(page, "server_confirm")).toHaveLength(0);
    });
  }

  test("launcher too old offers an update check", async ({ page }) => {
    await submitAddress(page, "future.example");
    await expect(
      page.getByRole("heading", { name: "Update vgames to use this server" }),
    ).toBeVisible();
    await expect(page.getByText("needs vgames 0.9.0 or newer. You have 0.4.0")).toBeVisible();
    await page.getByRole("button", { name: "Check for updates" }).click();
    await expect(page.getByRole("status")).toContainText("vgames 0.9.1 is available");
    await page.getByRole("button", { name: "Use a different server" }).click();
    await expect(page.getByRole("textbox", { name: /Server address/ })).toHaveValue("");
  });

  test("the user says the fingerprint does not match: nothing is saved", async ({ page }) => {
    await submitAddress(page, "games.example.org");
    await expect(page.getByTestId("fingerprint")).toContainText("7K2M");
    await page.getByRole("button", { name: "It doesn't match" }).click();
    await expect(page.getByRole("heading", { name: "Don't use this server" })).toBeVisible();
    expect(await commandCalls(page, "server_confirm")).toHaveLength(0);
  });
});

test("http://localhost is allowed in debug builds", async ({ page }) => {
  await open(page, "debug");
  await submitAddress(page, "http://localhost:8080");
  await expect(page.getByRole("heading", { name: "Check the server's fingerprint" })).toBeVisible();
});

test("an already added server can be switched to", async ({ page }) => {
  await open(page, "ready");
  await page.goto("/onboarding?add=1");
  await expect(page.getByRole("heading", { name: "Add a server" })).toBeVisible();
  await submitAddress(page, "https://games.example.org");
  await expect(page.getByRole("textbox", { name: /Server address/ })).toHaveAccessibleDescription(
    /already added/,
  );
  await page.getByRole("button", { name: "Switch to it" }).click();
  await expect(page).toHaveURL(/\/library$/);
});

test.describe("vgames://server/add links", () => {
  test("a mismatching fingerprint blocks the server", async ({ page }) => {
    await open(page, "fresh");
    await emit(page, "server-add-requested", {
      url: "https://games.example.org",
      fingerprint: OTHER_FINGERPRINT,
    });
    const alert = page.getByRole("alert").filter({ hasText: "Don't add this server" });
    await expect(alert).toBeVisible();
    await expect(alert).toContainText(OTHER_FINGERPRINT);
    await expect(alert).toContainText(FINGERPRINT);
    await expect(page.getByRole("button", { name: /It matches/ })).toHaveCount(0);
    expect(await commandCalls(page, "server_confirm")).toHaveLength(0);
    await page.getByRole("button", { name: "Start over" }).click();
    await expect(page.getByRole("heading", { name: "Welcome to vgames" })).toBeVisible();
  });

  test("a matching fingerprint is confirmed by the link", async ({ page }) => {
    await open(page, "ready");
    await expect(page.getByRole("heading", { name: "Library", level: 1 })).toBeVisible();
    await emit(page, "server-add-requested", {
      url: "https://paste.example",
      fingerprint: FINGERPRINT,
    });
    await expect(
      page.getByText("It matches the fingerprint in the link you opened."),
    ).toBeVisible();
    const preview = (await commandCalls(page, "server_preview"))[0] as Record<string, unknown>;
    expect(preview.expectedFingerprint).toBe(FINGERPRINT);
  });
});

test.describe("sign in", () => {
  const refusals: [string, string, RegExp][] = [
    ["registration closed", "closed.example", /isn't accepting new members right now/],
    ["not allowlisted", "invite-only.example", /isn't on this server's invite list/],
    ["account disabled", "disabled.example", /has been disabled/],
  ];
  for (const [name, host, message] of refusals) {
    test(name, async ({ page }) => {
      await open(page, "fresh");
      await reachSignIn(page, host);
      await page.getByRole("button", { name: "Sign in with Discord" }).click();
      await expect(page.getByRole("alert")).toContainText(message);
      await expect(page.getByRole("button", { name: "Sign in with Discord" })).toBeVisible();
    });
  }

  test("paste-code fallback: a wrong code, then the right one", async ({ page }) => {
    await open(page, "fresh");
    await reachSignIn(page, "paste.example");
    await page.getByRole("button", { name: "Sign in with Discord" }).click();
    await expect(
      page.getByRole("heading", { name: "Finish signing in in your browser" }),
    ).toBeVisible();
    await page.getByRole("button", { name: "Open browser again" }).click();
    expect(await commandCalls(page, "auth_open_browser")).toHaveLength(1);
    await page.getByRole("button", { name: "Paste a code instead" }).click();
    const code = page.getByRole("textbox", { name: "Sign-in code" });
    await expect(code).toBeFocused();
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await expect(code).toHaveAccessibleDescription(/Paste the code/);
    await code.fill("WRONG");
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await expect(code).toHaveAccessibleDescription(/That code didn't work/);
    await code.fill(VALID_CODE);
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "Choose where to install packages" }),
    ).toBeVisible();
  });

  test("browser cannot be opened: the code field is offered", async ({ page }) => {
    await open(page, "fresh");
    await reachSignIn(page, "nobrowser.example");
    await page.getByRole("button", { name: "Sign in with Discord" }).click();
    await expect(page.getByRole("alert")).toContainText("couldn't open your browser");
    await expect(page.getByRole("textbox", { name: "Sign-in code" })).toBeFocused();
  });

  test("cancelling sign-in returns to the start", async ({ page }) => {
    await open(page, "fresh");
    await reachSignIn(page, "paste.example");
    await page.getByRole("button", { name: "Sign in with Discord" }).click();
    await page.getByRole("button", { name: "Cancel sign-in" }).click();
    await expect(page.getByRole("button", { name: "Sign in with Discord" })).toBeVisible();
    expect(await commandCalls(page, "auth_cancel")).toHaveLength(1);
  });
});

test.describe("library folder", () => {
  test("an unusable folder shows why", async ({ page }) => {
    await open(page, "no-library", {
      libraryAddError: { kind: "nested_in_library", library_path: "/home/sam/Games" },
    });
    await page.getByRole("button", { name: "Choose folder" }).click();
    await expect(page.getByText("412 GB free of 931 GB")).toBeVisible();
    await page.getByRole("button", { name: "Use this folder" }).click();
    await expect(page.getByRole("alert")).toContainText("inside another library (/home/sam/Games)");
  });

  test("cancelling the folder picker changes nothing", async ({ page }) => {
    await open(page, "no-library", { folderPick: null });
    await page.getByRole("button", { name: "Choose folder" }).click();
    await expect(page.getByRole("button", { name: "Use this folder" })).toHaveCount(0);
  });
});

test("the whole first run works with the keyboard only", async ({ page }) => {
  await open(page, "fresh");
  const field = page.getByRole("textbox", { name: /Server address/ });
  await expect(field).toBeFocused();
  await page.keyboard.type("games.example.org");
  await page.keyboard.press("Enter");
  await expect(page.getByRole("heading", { name: "Check the server's fingerprint" })).toBeFocused();
  await page.keyboard.press("Tab");
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "It matches, continue" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("button", { name: "Sign in with Discord" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("button", { name: "Choose folder" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("button", { name: "Use this folder" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("button", { name: "Go to the library" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("heading", { name: "Library", level: 1 })).toBeVisible();
});

test("the whole first run works with a controller only", async ({ page }) => {
  await open(page, "fresh", {
    folderPick: { path: "/games", free_bytes: 1e12, total_bytes: 2e12 },
  });
  const pad = (action: string) =>
    emit(page, "ui-nav", { action, controller: "xinput", repeat: false });
  await page.getByRole("textbox", { name: /Server address/ }).fill("games.example.org");
  await pad("down");
  await expect(page.getByRole("button", { name: "Continue" })).toBeFocused();
  await pad("accept");
  await expect(page.getByRole("heading", { name: "Check the server's fingerprint" })).toBeFocused();
  await pad("down");
  await expect(page.getByRole("button", { name: "It doesn't match" })).toBeFocused();
  await pad("right");
  await expect(page.getByRole("button", { name: "It matches, continue" })).toBeFocused();
  await pad("accept");
  await expect(page.getByRole("button", { name: "Sign in with Discord" })).toBeFocused();
  await pad("accept");
  await expect(page.getByRole("button", { name: "Choose folder" })).toBeFocused();
  await pad("accept");
  await expect(page.getByRole("button", { name: "Use this folder" })).toBeFocused();
  await pad("accept");
  await expect(page.getByRole("button", { name: "Go to the library" })).toBeFocused();
  await pad("accept");
  await expect(page.getByRole("heading", { name: "Library", level: 1 })).toBeVisible();
});
