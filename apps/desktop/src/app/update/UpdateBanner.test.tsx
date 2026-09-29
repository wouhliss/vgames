import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { describe, expect, it } from "vitest";
import { events, type UpdaterStatus, type WhatsNew } from "../../ipc";
import {
  installMockBackend,
  MOCK_LIBRARY,
  MOCK_SERVER,
  MOCK_WHATS_NEW,
  type MockState,
} from "../../mocks/backend";
import { flush, renderWithProviders } from "../../test/render";
import { routes } from "../router";

const AVAILABLE: UpdaterStatus = {
  current_version: "0.4.0",
  state: { kind: "available", version: "0.9.1", date: "2026-09-20" },
  blocked: null,
};

function start(overrides: Partial<MockState> = {}, path = "/library") {
  const backend = installMockBackend({
    servers: [MOCK_SERVER],
    libraries: [MOCK_LIBRARY],
    updater: AVAILABLE,
    ...overrides,
  });
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  renderWithProviders(<RouterProvider router={router} />);
  return { backend, user: userEvent.setup() };
}

async function openWhatsNew(user: ReturnType<typeof userEvent.setup>) {
  const banner = await screen.findByRole("region", { name: "vgames 0.9.1 is available" });
  await user.click(within(banner).getByRole("button", { name: "What's new" }));
  return screen.findByRole("dialog", { name: "What's new in vgames 0.9.1" });
}

async function emitStatus(status: UpdaterStatus) {
  await act(async () => {
    await events.updaterStatus.emit(status);
    await flush();
  });
}

describe("update banner", () => {
  it("stays hidden when vgames is up to date", async () => {
    start({ updater: { ...AVAILABLE, state: { kind: "up_to_date" } } });
    await screen.findByRole("heading", { level: 1, name: "Library" });
    expect(screen.queryByText(/is available/)).not.toBeInTheDocument();
  });

  it("appears when the core reports an update, and Later hides it for that version", async () => {
    const { user } = start({ updater: { ...AVAILABLE, state: { kind: "up_to_date" } } });
    await screen.findByRole("heading", { level: 1, name: "Library" });
    await emitStatus(AVAILABLE);
    const banner = screen.getByRole("region", { name: "vgames 0.9.1 is available" });
    await user.click(within(banner).getByRole("button", { name: "Later" }));
    expect(screen.queryByRole("region", { name: /is available/ })).not.toBeInTheDocument();
    // A newer version shows the banner again.
    await emitStatus({ ...AVAILABLE, state: { kind: "available", version: "0.9.2", date: null } });
    expect(screen.getByRole("region", { name: "vgames 0.9.2 is available" })).toBeInTheDocument();
  });

  it("groups What's new by type, as plain text, with the generic line for an empty release", async () => {
    const withMarkup: WhatsNew = {
      ...MOCK_WHATS_NEW,
      releases: [
        {
          version: "0.9.1",
          date: "2026-09-20",
          entries: [
            ...(MOCK_WHATS_NEW.releases[0]?.entries ?? []),
            { type: "fixed", text: "<b>Bold</b> **not markdown**" },
          ],
        },
        { version: "0.9.0", date: "2026-09-10", entries: [] },
      ],
    };
    const { user } = start({ whatsNew: withMarkup });
    const dialog = await openWhatsNew(user);
    const notes = await within(dialog).findByRole("region", { name: "What's new" });
    const headings = within(notes)
      .getAllByRole("heading", { level: 4 })
      .map((h) => h.textContent);
    expect(headings).toEqual(["New", "Improved", "Fixed", "Removed", "Security"]);
    expect(within(notes).getByText("<b>Bold</b> **not markdown**")).toBeInTheDocument();
    expect(notes.querySelector("b, strong")).toBeNull();

    const empty = within(notes).getByRole("region", { name: "vgames 0.9.0" });
    expect(within(empty).queryAllByRole("listitem")).toHaveLength(0);
    expect(within(empty).getByText("Stability and performance improvements.")).toBeInTheDocument();
  });

  it("renders a release with zero user entries as the single generic line", async () => {
    const { user } = start({
      whatsNew: {
        from_latest_notes: false,
        releases: [{ version: "0.9.1", date: "2026-09-20", entries: [] }],
      },
    });
    const dialog = await openWhatsNew(user);
    const notes = await within(dialog).findByRole("region", { name: "What's new" });
    expect(within(notes).queryAllByRole("listitem")).toHaveLength(0);
    expect(within(notes).queryAllByRole("heading", { level: 4 })).toHaveLength(0);
    expect(within(notes).getAllByText("Stability and performance improvements.")).toHaveLength(1);
  });

  it("shows notes from latest.json as a plain list, without groups", async () => {
    const { user } = start({
      whatsNew: {
        from_latest_notes: true,
        releases: [
          { version: "0.9.1", date: "", entries: [{ type: "changed", text: "Faster library." }] },
        ],
      },
    });
    const dialog = await openWhatsNew(user);
    const notes = await within(dialog).findByRole("region", { name: "What's new" });
    expect(within(notes).getByText("Faster library.")).toBeInTheDocument();
    expect(within(notes).queryByText("Improved")).not.toBeInTheDocument();
    expect(within(notes).queryByText(/Released/)).not.toBeInTheDocument();
  });

  it("keeps a long changelog in a focusable scrolling region", async () => {
    const releases = Array.from({ length: 30 }, (_, i) => ({
      version: `0.${9 - Math.floor(i / 10)}.${i}`,
      date: "2026-09-01",
      entries: Array.from({ length: 12 }, (_, j) => ({
        type: "fixed" as const,
        text: `Fixed thing ${i}-${j}.`,
      })),
    }));
    const { user, backend } = start({ whatsNew: { from_latest_notes: false, releases } });
    // The notes are loaded in the background once an update is known.
    await waitFor(() => expect(backend.callsTo("updater_whats_new")).toHaveLength(1));
    await flush();
    const dialog = await openWhatsNew(user);
    const notes = await within(dialog).findByRole("region", { name: "What's new" });
    expect(notes).toHaveAttribute("tabindex", "0");
    expect(notes.className).toMatch(/notes/);
    expect(within(notes).getAllByRole("listitem")).toHaveLength(360);
    // Dialog focus starts in the notes, so arrow keys and the D-pad scroll them.
    expect(notes).toHaveFocus();
    // The footer stays reachable.
    expect(within(dialog).getByRole("button", { name: "Install and restart" })).toBeVisible();
  });

  it("offers a retry when What's new can't be loaded", async () => {
    const { user, backend } = start({
      whatsNew: { error: { code: "offline", message: "The server could not be reached." } },
    });
    const dialog = await openWhatsNew(user);
    await within(dialog).findByText("Couldn't load what's new.");
    backend.state.whatsNew = MOCK_WHATS_NEW;
    await user.click(within(dialog).getByRole("button", { name: "Try again" }));
    expect(await within(dialog).findByRole("region", { name: "What's new" })).toBeInTheDocument();
  });

  it("disables Install and restart with a reason while a game runs", async () => {
    const { user, backend } = start({ updater: { ...AVAILABLE, blocked: "game_running" } });
    const dialog = await openWhatsNew(user);
    const install = within(dialog).getByRole("button", { name: "Install and restart" });
    expect(install).toHaveAttribute("aria-disabled", "true");
    expect(install).toHaveAccessibleDescription("Close your game to install the update.");
    await user.click(install);
    expect(backend.callsTo("updater_install")).toHaveLength(0);
    // The game closes: the button works again.
    await emitStatus(AVAILABLE);
    expect(install).not.toHaveAttribute("aria-disabled");
    expect(install).not.toHaveAccessibleDescription();
  });

  it("explains that downloads pause first, without blocking the install", async () => {
    const { user } = start({ updater: { ...AVAILABLE, blocked: "downloads_active" } });
    const dialog = await openWhatsNew(user);
    const install = within(dialog).getByRole("button", { name: "Install and restart" });
    expect(install).not.toHaveAttribute("aria-disabled");
    expect(install).toHaveAccessibleDescription(/Downloads pause at a safe point first/);
  });

  it("installs: disabled while installing, progress in the banner, then restarting", async () => {
    const { user, backend } = start();
    const dialog = await openWhatsNew(user);
    await user.click(within(dialog).getByRole("button", { name: "Install and restart" }));
    await waitFor(() => expect(backend.callsTo("updater_install")).toHaveLength(1));
    expect(await screen.findByText("vgames 0.9.1 is installed. Restarting…")).toBeInTheDocument();
    const button = within(dialog).getByRole("button", { name: /Installing/ });
    expect(button).toHaveAttribute("aria-disabled", "true");
    expect(button).toHaveAccessibleDescription("The update is already being installed.");
  });

  it("shows download progress in the banner", async () => {
    start();
    await screen.findByRole("region", { name: "vgames 0.9.1 is available" });
    await emitStatus({
      ...AVAILABLE,
      state: { kind: "downloading", version: "0.9.1", downloaded: 512, total: 1024 },
    });
    expect(screen.getByText("Downloading vgames 0.9.1…")).toBeInTheDocument();
    expect(screen.getByRole("progressbar", { name: "Update download" })).toHaveAttribute(
      "aria-valuenow",
      "50",
    );
  });

  it("shows a refused install in the dialog", async () => {
    const { user } = start({
      updateInstallError: { code: "conflict", message: "Close the game to install the update." },
    });
    const dialog = await openWhatsNew(user);
    await user.click(within(dialog).getByRole("button", { name: "Install and restart" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "Couldn't install the update: Close the game to install the update.",
    );
  });

  it("reports a failed install until dismissed, but not a failed background check", async () => {
    const { user } = start();
    await screen.findByRole("region", { name: "vgames 0.9.1 is available" });
    // A background check that fails says nothing.
    await emitStatus({ ...AVAILABLE, state: { kind: "failed", message: "Could not check." } });
    expect(screen.queryByText("The update wasn't installed")).not.toBeInTheDocument();

    await emitStatus({
      ...AVAILABLE,
      state: { kind: "downloading", version: "0.9.1", downloaded: 0, total: null },
    });
    await emitStatus({
      ...AVAILABLE,
      state: {
        kind: "failed",
        message: "The update failed security checks and was not installed.",
      },
    });
    const alert = screen.getByText("The update wasn't installed").closest("[role=alert]");
    if (!(alert instanceof HTMLElement)) throw new Error("no alert");
    expect(alert).toHaveTextContent("The update failed security checks and was not installed.");
    await user.click(within(alert).getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByText("The update wasn't installed")).not.toBeInTheDocument();
  });

  it("opens What's new from Settings → Updates", async () => {
    const { user } = start({}, "/settings/updates");
    await user.click(await screen.findByRole("button", { name: "What's new in 0.9.1" }));
    const dialogs = await screen.findAllByRole("dialog", { name: "What's new in vgames 0.9.1" });
    expect(dialogs).toHaveLength(1);
  });
});
