import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { routes } from "../../app/router";
import type { Library, ServerProfile } from "../../ipc";
import {
  installMockBackend,
  MOCK_LIBRARY,
  MOCK_SERVER,
  type MockState,
  OTHER_FINGERPRINT,
} from "../../mocks/backend";
import { makeInstalls, OFFLINE_LIBRARY } from "../../mocks/library";
import { renderWithProviders } from "../../test/render";
import { parseLimit } from "./DownloadsSection";

const KEY = "vgames.test.settings";
const GiB = 1024 ** 3;

const OTHER: ServerProfile = {
  ...MOCK_SERVER,
  id: "01920000-0000-7000-8000-000000000002",
  url: "https://lan.example.org",
  name: "LAN Party",
  fingerprint: OTHER_FINGERPRINT,
  active: false,
  account: {
    ...MOCK_SERVER.account,
    username: "sam-lan",
    display_name: null,
  } as ServerProfile["account"],
};

const SSD: Library = {
  ...MOCK_LIBRARY,
  id: "01920000-0000-7000-8000-0000000000b3",
  path: "/mnt/ssd",
  label: "Fast SSD",
  is_default: false,
  free_bytes: 900 * GiB,
  total_bytes: 1000 * GiB,
};

const BASE: Partial<MockState> = {
  servers: [MOCK_SERVER, OTHER],
  libraries: [MOCK_LIBRARY, SSD],
  installs: [],
  persistKey: KEY,
  sessions: {
    [MOCK_SERVER.id]: [
      {
        id: "s1",
        device_name: "vgames 0.4 on Linux",
        platform: "linux",
        created_at: "2026-09-01T10:00:00Z",
        last_used_at: "2026-09-26T10:00:00Z",
        current: true,
      },
      {
        id: "s2",
        device_name: "Firefox on Windows",
        platform: "web",
        created_at: "2026-08-01T10:00:00Z",
        last_used_at: null,
        current: false,
      },
    ],
  },
};

function start(path: string, overrides: Partial<MockState> = {}) {
  const backend = installMockBackend({ ...BASE, ...overrides });
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  renderWithProviders(<RouterProvider router={router} />);
  return { backend, router, user: userEvent.setup() };
}

/** Closes the app and starts it again on the same (mock) storage. */
function restart(path: string, overrides: Partial<MockState> = {}) {
  cleanup();
  return start(path, overrides);
}

const section = (name: string) => screen.findByRole("region", { name });

beforeEach(() => {
  localStorage.clear();
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(1000);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(800);
});

describe("settings", () => {
  it("opens on General and switches sections through the list", async () => {
    const { user, router } = start("/settings");
    expect(await section("General")).toBeVisible();
    await waitFor(() => expect(router.state.location.pathname).toBe("/settings/general"));
    const nav = screen.getByRole("navigation", { name: "Settings sections" });
    expect(within(nav).getByRole("link", { name: "General" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    await user.click(within(nav).getByRole("link", { name: "Storage" }));
    expect(await section("Storage")).toBeVisible();
    expect(router.state.location.pathname).toBe("/settings/storage");
  });

  describe("general", () => {
    it("keeps the theme and reduced motion after a restart", async () => {
      const { user, backend } = start("/settings/general");
      const themes = await screen.findByRole("radiogroup", { name: "Theme" });
      await user.click(within(themes).getByRole("radio", { name: /High contrast/ }));
      await user.click(screen.getByRole("switch", { name: "Reduce motion" }));
      await waitFor(() =>
        expect(backend.state.appearance).toEqual({ theme: "high_contrast", reduce_motion: true }),
      );

      restart("/settings/general");
      const again = await screen.findByRole("radiogroup", { name: "Theme" });
      expect(within(again).getByRole("radio", { name: /High contrast/ })).toBeChecked();
      expect(screen.getByRole("switch", { name: "Reduce motion" })).toHaveAttribute(
        "aria-checked",
        "true",
      );
    });
  });

  describe("servers", () => {
    it("lists servers with their address and fingerprint", async () => {
      start("/settings/servers");
      await section("Servers");
      const friday = screen.getByRole("listitem", { name: "Friday Night Games" });
      expect(within(friday).getByText("In use")).toBeVisible();
      expect(friday).toHaveTextContent("https://games.example.org");
      expect(within(friday).getByTestId("fingerprint")).toHaveTextContent(
        MOCK_SERVER.fingerprint.split("-").join(""),
      );
      expect(friday).toHaveTextContent("Signed in as Sam");
      expect(screen.getByRole("listitem", { name: "LAN Party" })).toHaveTextContent(
        "Signed in as sam-lan",
      );
    });

    it("switches servers, and remembers it after a restart", async () => {
      const { user } = start("/settings/servers");
      await section("Servers");
      await user.click(screen.getByRole("button", { name: "Use LAN Party" }));
      expect(await screen.findByText("Switched to LAN Party")).toBeVisible();
      restart("/settings/servers");
      await section("Servers");
      expect(
        within(screen.getByRole("listitem", { name: "LAN Party" })).getByText("In use"),
      ).toBeVisible();
    });

    it("removes a server after confirmation", async () => {
      const { user, backend } = start("/settings/servers");
      await section("Servers");
      await user.click(screen.getByRole("button", { name: "Remove LAN Party…" }));
      const dialog = screen.getByRole("alertdialog", { name: "Remove LAN Party?" });
      await user.click(within(dialog).getByRole("button", { name: "Remove" }));
      expect(await screen.findByText("LAN Party removed")).toBeVisible();
      expect(backend.state.servers.map((s) => s.id)).toEqual([MOCK_SERVER.id]);
    });

    it("adds a server through the first-run flow", async () => {
      const { user, router } = start("/settings/servers");
      await section("Servers");
      await user.click(screen.getByRole("button", { name: "Add a server…" }));
      await waitFor(() => expect(router.state.location.search).toBe("?add=1"));
    });
  });

  describe("account", () => {
    it("lists where you're signed in and signs out another device", async () => {
      const { user, backend } = start("/settings/account");
      await section("Account");
      expect(screen.getByText("Signed in to Friday Night Games as Sam")).toBeVisible();
      const current = (await screen.findByText("vgames 0.4 on Linux")).closest("li");
      if (!current) throw new Error("row");
      expect(within(current).getByText("This device")).toBeVisible();
      expect(within(current).queryByRole("button")).toBeNull();
      await user.click(screen.getByRole("button", { name: "Sign out Firefox on Windows" }));
      await user.click(
        within(screen.getByRole("alertdialog", { name: "Sign out Firefox on Windows?" })).getByRole(
          "button",
          { name: "Sign out" },
        ),
      );
      expect(await screen.findByText("Firefox on Windows signed out")).toBeVisible();
      expect(backend.state.sessions[MOCK_SERVER.id]?.map((s) => s.id)).toEqual(["s1"]);
      expect(screen.queryByText("Firefox on Windows", { selector: "span" })).toBeNull();
    });

    it("warns when sign-in tokens are kept in a file", async () => {
      start("/settings/account", {
        credentialStorage: { kind: "file_fallback", path: "/home/sam/.config/vgames/tokens" },
      });
      expect(await screen.findByText("Your sign-in is stored in a file")).toBeVisible();
      expect(
        screen.getByText(/keeps your sign-in in \/home\/sam\/\.config\/vgames\/tokens/),
      ).toBeVisible();
    });

    it("signs out of the active server", async () => {
      const { user, backend } = start("/settings/account");
      await section("Account");
      await user.click(screen.getByRole("button", { name: "Sign out" }));
      await user.click(
        within(
          screen.getByRole("alertdialog", { name: "Sign out of Friday Night Games?" }),
        ).getByRole("button", { name: "Sign out" }),
      );
      await waitFor(() => expect(backend.state.servers[0]?.account).toBeNull());
    });
  });

  describe("storage", () => {
    it("shows free space and the default library", async () => {
      start("/settings/storage", {
        libraries: [MOCK_LIBRARY, SSD, OFFLINE_LIBRARY],
        installs: makeInstalls(3, MOCK_SERVER, [MOCK_LIBRARY]),
      });
      await section("Storage");
      const home = screen.getByRole("listitem", { name: "/home/sam/Games" });
      expect(within(home).getByText("Default")).toBeVisible();
      await waitFor(() => expect(home).toHaveTextContent("412 GB free of 931 GB · 3 packages"));
      expect(within(home).getByRole("progressbar")).toHaveAttribute("aria-valuenow", "56");
      const external = screen.getByRole("listitem", { name: "External drive" });
      expect(within(external).getByText("Drive not connected")).toBeVisible();
      // Its files can't be reached: it can only be removed.
      expect(
        within(external)
          .getAllByRole("button")
          .map((b) => b.textContent),
      ).toEqual(["Remove…"]);
    });

    it("changes the default library, and remembers it after a restart", async () => {
      const { user } = start("/settings/storage");
      await section("Storage");
      await user.click(screen.getByRole("button", { name: "Make /mnt/ssd the default library" }));
      expect(await screen.findByText("New installs now go to /mnt/ssd")).toBeVisible();
      restart("/settings/storage");
      await section("Storage");
      expect(
        within(screen.getByRole("listitem", { name: "Fast SSD" })).getByText("Default"),
      ).toBeVisible();
    });

    it("explains why a library can't be removed, and removes an empty one", async () => {
      const { user, backend } = start("/settings/storage", {
        installs: makeInstalls(2, MOCK_SERVER, [SSD]),
      });
      await section("Storage");
      const confirm = async (path: string) => {
        await user.click(screen.getByRole("button", { name: `Remove the library ${path}…` }));
        await user.click(
          within(
            screen.getByRole("alertdialog", { name: `Remove the library ${path}?` }),
          ).getByRole("button", { name: "Remove" }),
        );
      };
      await confirm("/mnt/ssd");
      const storage = screen.getByRole("region", { name: "Storage" });
      expect(await within(storage).findByRole("alert")).toHaveTextContent(
        "2 packages are still installed there. Move or uninstall them first.",
      );
      await confirm("/home/sam/Games");
      await waitFor(() =>
        expect(within(storage).getByRole("alert")).toHaveTextContent(
          "This is the default library. Make another library the default first.",
        ),
      );
      backend.state.installs = [];
      await confirm("/mnt/ssd");
      expect(await screen.findByText("/mnt/ssd removed")).toBeVisible();
    });

    it("adds a library, and explains a folder that can't be used", async () => {
      const { user, backend } = start("/settings/storage", {
        folderPick: { path: "/home/sam/Games/nested", free_bytes: GiB, total_bytes: 10 * GiB },
        libraryAddError: { kind: "nested_in_library", library_path: "/home/sam/Games" },
      });
      await section("Storage");
      await user.click(screen.getByRole("button", { name: "Add a library…" }));
      const storage = screen.getByRole("region", { name: "Storage" });
      expect(await within(storage).findByRole("alert")).toHaveTextContent(
        "inside another library (/home/sam/Games)",
      );
      backend.state.libraryAddError = null;
      backend.state.folderPick = { path: "/data/games", free_bytes: GiB, total_bytes: 10 * GiB };
      await user.click(screen.getByRole("button", { name: "Add a library…" }));
      expect(await screen.findByText("/data/games added")).toBeVisible();
      expect(screen.getByRole("listitem", { name: "/data/games" })).toBeVisible();
    });

    it("moves every package of a library to another one", async () => {
      const installs = makeInstalls(2, MOCK_SERVER, [MOCK_LIBRARY]);
      const { user, backend } = start("/settings/storage", { installs, actionDelayMs: 0 });
      await section("Storage");
      await user.click(
        await screen.findByRole("button", { name: "Move the packages in /home/sam/Games…" }),
      );
      const dialog = screen.getByRole("dialog", { name: "Move 2 packages from /home/sam/Games" });
      expect(within(dialog).getByRole("radio", { name: /Fast SSD/ })).toBeChecked();
      await user.click(within(dialog).getByRole("button", { name: "Move" }));
      expect(await screen.findByText("Moving 2 packages…")).toBeVisible();
      expect(backend.callsTo("install_move").map((c) => c.args.libraryId)).toEqual([
        SSD.id,
        SSD.id,
      ]);
    });
  });

  describe("downloads", () => {
    it("parses speed limits in MB/s", () => {
      expect(parseLimit("10")).toBe(9766);
      expect(parseLimit("0,5")).toBe(488);
      expect(parseLimit("0.05")).toBeNull();
      expect(parseLimit("1001")).toBeNull();
      expect(parseLimit("ten")).toBeNull();
      expect(parseLimit("")).toBeNull();
    });

    it("validates the speed limit and keeps both settings after a restart", async () => {
      const { user, backend } = start("/settings/downloads");
      await section("Downloads");
      await user.click(await screen.findByRole("switch", { name: "Limit download speed" }));
      const field = await screen.findByRole("textbox", { name: "Maximum speed (MB/s)" });
      expect(field).toHaveValue("10");
      await user.clear(field);
      await user.type(field, "5000{Enter}");
      expect(await screen.findByText("Enter a speed between 0.1 and 1000 MB/s.")).toBeVisible();
      expect(backend.state.downloadSettings.bandwidth_limit_kib).toBe(9766);
      await user.clear(field);
      await user.type(field, "2.5{Enter}");
      await waitFor(() => expect(backend.state.downloadSettings.bandwidth_limit_kib).toBe(2441));
      expect(screen.queryByText("Enter a speed between 0.1 and 1000 MB/s.")).toBeNull();
      await user.click(screen.getByRole("radio", { name: /2 at a time/ }));
      await waitFor(() => expect(backend.state.downloadSettings.concurrent_installs).toBe(2));

      restart("/settings/downloads");
      expect(await screen.findByRole("textbox", { name: "Maximum speed (MB/s)" })).toHaveValue(
        "2.5",
      );
      expect(screen.getByRole("radio", { name: /2 at a time/ })).toBeChecked();
    });
  });

  describe("updates", () => {
    it.each([
      [{ kind: "up_to_date" as const }, "vgames is up to date."],
      [{ kind: "available" as const, version: "0.9.1" }, "Version 0.9.1 is available."],
      [{ kind: "failed" as const, detail: "timed out" }, "Couldn't check for updates: timed out"],
    ])("checks now: %o", async (check, text) => {
      const { user } = start("/settings/updates", { updateCheck: check });
      expect(await screen.findByText("vgames 0.4.0")).toBeVisible();
      await user.click(screen.getByRole("button", { name: "Check for updates" }));
      expect(await screen.findByText(text)).toBeVisible();
    });
  });

  describe("about", () => {
    it("shows the version and the licenses as plain text", async () => {
      const { user } = start("/settings/about", {
        licenses: "Some library — MIT\n<b>not bold</b>",
      });
      expect(await screen.findByText("Version 0.4.0")).toBeVisible();
      await user.click(screen.getByRole("button", { name: "Third-party licenses" }));
      const dialog = await screen.findByRole("dialog", { name: "Third-party licenses" });
      expect(await within(dialog).findByText(/Some library — MIT/)).toHaveTextContent(
        "<b>not bold</b>",
      );
      expect(dialog.querySelector("b")).toBeNull();
    });

    it("copies the diagnostics", async () => {
      const { user } = start("/settings/about");
      await user.click(await screen.findByRole("button", { name: "Copy diagnostics" }));
      expect(await screen.findByText("Diagnostics copied")).toBeVisible();
      // user-event provides the clipboard in tests.
      expect(await navigator.clipboard.readText()).toContain("vgames 0.4.0");
    });
  });
});
