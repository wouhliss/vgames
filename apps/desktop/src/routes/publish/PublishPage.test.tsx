// INS-06 screen fixtures (src/mocks/publishing.ts): a release end to end, and the four failures the
// task names (wrong passphrase, untrusted key, cancelled then resumed, failed verification), plus
// invalid entries and the route being absent for players.
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { describe, expect, it } from "vitest";
import { routes } from "../../app/router";
import {
  installMockBackend,
  MOCK_ACCOUNT,
  MOCK_LIBRARY,
  MOCK_SERVER,
  type MockState,
} from "../../mocks/backend";
import { PUBLISH_FOLDERS, PUBLISH_KEYS } from "../../mocks/publishing";
import { renderWithProviders } from "../../test/render";

const ADMIN_SERVER = { ...MOCK_SERVER, account: { ...MOCK_ACCOUNT, role: "admin" as const } };
type User = ReturnType<typeof userEvent.setup>;

function start(overrides: Partial<MockState> = {}, path = "/publish") {
  const backend = installMockBackend({
    servers: [ADMIN_SERVER],
    libraries: [MOCK_LIBRARY],
    ...overrides,
  });
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  renderWithProviders(<RouterProvider router={router} />);
  return { backend, router, user: userEvent.setup() };
}

async function pick(user: User, name: string, option: string | RegExp) {
  await user.click(await screen.findByRole("combobox", { name }));
  await user.click(await screen.findByRole("option", { name: option }));
}

/** Fills the form for Starfall and presses Upload. */
async function upload(user: User, passphrase = "correct horse") {
  await screen.findByRole("heading", { level: 1, name: "Publish" });
  await pick(user, "Package", /^Starfall\b/);
  await user.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByText(/files · .+ · .+ packs/);
  await pick(user, "Platform", "Windows (x64)");
  await user.type(screen.getByRole("textbox", { name: /Version name/ }), "1.4.0");
  await user.click(screen.getByRole("button", { name: "Choose key file" }));
  await user.type(screen.getByLabelText("Key passphrase"), passphrase);
  await user.click(screen.getByRole("button", { name: "Upload" }));
}

/** The upload's list item, once it is listed. */
async function jobItem(title = "Starfall 1.4.0"): Promise<HTMLElement> {
  const heading = await screen.findByRole("heading", { level: 3, name: title });
  const item = heading.closest("li");
  if (!item) throw new Error("the job heading is not in a list item");
  return item;
}

describe("publish screen", () => {
  it("is absent for players: no sidebar entry, and the route goes to the library", async () => {
    start({ servers: [MOCK_SERVER] });
    await screen.findByRole("heading", { level: 1, name: "Library" });
    const nav = screen.getByRole("navigation", { name: "Main" });
    expect(within(nav).queryByRole("link", { name: "Publish" })).not.toBeInTheDocument();
  });

  it("shows the sidebar entry to admins", async () => {
    start({}, "/library");
    const nav = await screen.findByRole("navigation", { name: "Main" });
    expect(within(nav).getByRole("link", { name: "Publish" })).toBeInTheDocument();
  });

  it("uploads, checks and releases a version", async () => {
    const { user, backend } = start();
    await upload(user);
    const item = await jobItem();
    await waitFor(() =>
      expect(within(item).getAllByText("Checked, not released").length).toBeGreaterThan(0),
    );
    await user.click(within(item).getByRole("button", { name: "Release Starfall 1.4.0" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Release Starfall 1.4.0?" });
    await user.click(within(dialog).getByRole("button", { name: "Release" }));
    expect(await screen.findByText("Starfall 1.4.0 is released.")).toBeInTheDocument();
    expect(backend.callsTo("publish_start")[0]?.args).toMatchObject({
      start: {
        key_path: PUBLISH_KEYS.ok,
        folder: PUBLISH_FOLDERS.ok,
        launch: { executable: "Starfall.exe" },
      },
    });
    const versions = screen.getByRole("region", { name: "Versions of Starfall" });
    await waitFor(() =>
      expect(within(versions).getAllByText("Current release").length).toBeGreaterThan(0),
    );
  });

  it("shows a wrong passphrase on the passphrase field and starts nothing", async () => {
    const { user, backend } = start();
    await upload(user, "wrong");
    expect(await screen.findByText("That passphrase doesn't unlock this key.")).toBeInTheDocument();
    expect(screen.getByLabelText("Key passphrase")).toHaveAttribute("aria-invalid", "true");
    expect(backend.state.publishJobs).toHaveLength(0);
  });

  it("explains an untrusted key", async () => {
    const { user, backend } = start({ publishKeyPick: PUBLISH_KEYS.untrusted });
    await upload(user);
    expect(
      await screen.findByText("This server doesn't know this key. Ask the server owner to add it."),
    ).toBeInTheDocument();
    expect(backend.state.publishJobs).toHaveLength(0);
  });

  it("stops an upload and resumes it with the key", async () => {
    const { user, backend } = start({ publishHold: true });
    await upload(user);
    const item = await jobItem();
    await user.click(within(item).getByRole("button", { name: "Stop Starfall 1.4.0" }));
    await waitFor(() => expect(within(item).getByText(/^Stopped · /)).toBeInTheDocument());
    backend.state.publishHold = false;
    await user.click(within(item).getByRole("button", { name: "Resume Starfall 1.4.0" }));
    const dialog = await screen.findByRole("dialog", { name: "Unlock your key to resume" });
    await user.click(within(dialog).getByRole("button", { name: "Choose key file" }));
    await user.type(within(dialog).getByLabelText("Key passphrase"), "correct horse");
    await user.click(within(dialog).getByRole("button", { name: "Continue" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "Unlock your key to resume" })).toBeNull(),
    );
    await waitFor(() =>
      expect(within(item).getAllByText("Checked, not released").length).toBeGreaterThan(0),
    );
  });

  it("reports a failed verification on the upload", async () => {
    const { user } = start({ publishFolderPick: PUBLISH_FOLDERS.failsVerification });
    await upload(user);
    expect(
      await screen.findByText(
        "The server found files that don't match what was uploaded: Pack 1 does not match its hash.",
      ),
    ).toBeInTheDocument();
    const item = await jobItem();
    expect(within(item).queryByRole("button", { name: /^Resume/ })).toBeNull();
  });

  it("lists entries that can't be published and blocks the upload", async () => {
    const { user, backend } = start({ publishFolderPick: PUBLISH_FOLDERS.invalid });
    await screen.findByRole("heading", { level: 1, name: "Publish" });
    await pick(user, "Package", /^Starfall\b/);
    await user.click(screen.getByRole("button", { name: "Choose folder" }));
    const title = await screen.findByText("3 files can't be published");
    const alert = title.closest<HTMLElement>('[role="alert"]');
    if (!alert) throw new Error("the invalid entries are not announced");
    expect(
      within(alert).getByText(/differs only by letter case from data\/save\.dat/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Upload" })).toHaveAttribute("aria-disabled", "true");
    expect(backend.callsTo("publish_start")).toHaveLength(0);
  });
});
