import { act, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { describe, expect, it } from "vitest";
import { events } from "../ipc";
import { installMockBackend, MOCK_LIBRARY, MOCK_SERVER, OTHER_FINGERPRINT } from "../mocks/backend";
import { flush, renderWithProviders } from "../test/render";
import { CrashScreen } from "./ErrorBoundary";
import { routes } from "./router";

function renderApp(path = "/library") {
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  renderWithProviders(<RouterProvider router={router} />);
  return router;
}

const SECOND = {
  ...MOCK_SERVER,
  id: "01920000-0000-7000-8000-000000000002",
  name: "Backup Server",
  active: false,
};

describe("app shell", () => {
  it("sends first-run users to onboarding", async () => {
    installMockBackend();
    const router = renderApp();
    await waitFor(() => expect(router.state.location.pathname).toBe("/onboarding"));
    expect(await screen.findByRole("heading", { name: "Welcome to vgames" })).toBeInTheDocument();
  });

  it("shows navigation, server switcher and account menu when ready", async () => {
    installMockBackend({ servers: [MOCK_SERVER], libraries: [MOCK_LIBRARY] });
    renderApp();
    const nav = await screen.findByRole("navigation", { name: "Main" });
    for (const name of ["Library", "Browse", "Friends", "Downloads", "Settings"]) {
      expect(screen.getAllByRole("link", { name }).length).toBeGreaterThan(0);
    }
    expect(nav).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Server: Friday Night Games" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Account: Sam" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { level: 1, name: "Library" })).toBeInTheDocument();
  });

  it("shows and clears the offline banner from core events", async () => {
    installMockBackend({ servers: [MOCK_SERVER], libraries: [MOCK_LIBRARY] });
    renderApp();
    await screen.findByRole("heading", { level: 1, name: "Library" });
    await act(async () => {
      await events.connectivityChanged.emit({ server_id: MOCK_SERVER.id, online: false });
      await flush();
    });
    expect(screen.getByText("You're offline").closest("[role=status]")).toBeInTheDocument();
    await act(async () => {
      await events.connectivityChanged.emit({ server_id: MOCK_SERVER.id, online: true });
      await flush();
    });
    expect(screen.queryByText("You're offline")).not.toBeInTheDocument();
  });

  it("blocks the app on a fingerprint mismatch with no way to dismiss", async () => {
    const backend = installMockBackend({
      servers: [MOCK_SERVER, SECOND],
      libraries: [MOCK_LIBRARY],
    });
    const user = userEvent.setup();
    renderApp();
    await screen.findByRole("heading", { level: 1, name: "Library" });
    await act(async () => {
      await events.trustProblem.emit({
        server_id: MOCK_SERVER.id,
        kind: "fingerprint_mismatch",
        server_name: MOCK_SERVER.name,
        pinned_fingerprint: MOCK_SERVER.fingerprint,
        presented_fingerprint: OTHER_FINGERPRINT,
      });
      await flush();
    });
    const block = screen.getByRole("alertdialog", { name: "This server's identity has changed" });
    expect(block).toHaveTextContent(OTHER_FINGERPRINT);
    expect(
      screen.getByRole("heading", { name: "This server's identity has changed" }),
    ).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(block).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /close|dismiss|continue/i }),
    ).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Use Backup Server instead" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
    expect(backend.callsTo("server_switch")[0]?.args).toEqual({ serverId: SECOND.id });
  });

  it("the crash screen copies redacted diagnostics", async () => {
    installMockBackend();
    // user-event installs a clipboard stub we can read back.
    const user = userEvent.setup();
    renderWithProviders(<CrashScreen error={new TypeError("x is undefined")} />);
    await user.click(screen.getByRole("button", { name: "Copy details" }));
    expect(await screen.findByText(/Diagnostics copied/)).toBeInTheDocument();
    const copied = await navigator.clipboard.readText();
    expect(copied).toContain("vgames 0.4.0");
    expect(copied).toContain("TypeError: x is undefined");
  });
});
