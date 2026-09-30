// A3-T11: the admin publishing screen, driven by the mock core. Covers the plan preview (invalid paths
// block), wrong passphrase, untrusted key, cancel and resume, verification failure, publish and yank.
import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent, { type UserEvent } from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { routes } from "../../app/router";
import { events } from "../../ipc";
import {
  installMockBackend,
  MOCK_ACCOUNT,
  MOCK_LIBRARY,
  MOCK_SERVER,
  type MockState,
} from "../../mocks/backend";
import { CORRECT_PASSPHRASE, DEFAULT_PLAN, type PublishScenario } from "../../mocks/publish";
import { flush, renderWithProviders } from "../../test/render";

const ADMIN = { ...MOCK_SERVER, account: { ...MOCK_ACCOUNT, role: "admin" as const } };

function start(
  publish: Partial<MockState["publish"]> = {},
  role: "user" | "admin" | "owner" = "admin",
) {
  const backend = installMockBackend({
    servers: [{ ...ADMIN, account: { ...MOCK_ACCOUNT, role } }],
    libraries: [MOCK_LIBRARY],
    installs: [],
  });
  Object.assign(backend.state.publish, publish);
  const router = createMemoryRouter(routes, { initialEntries: ["/publish"] });
  renderWithProviders(<RouterProvider router={router} />);
  return { backend, user: userEvent.setup() };
}

beforeEach(() => {
  localStorage.clear();
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(1000);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(800);
});

const startButton = () => screen.getByRole("button", { name: "Upload and verify" });

async function fill(
  user: UserEvent,
  { label = "1.2.0", passphrase = CORRECT_PASSPHRASE, key = true, folder = true } = {},
) {
  await user.click(await screen.findByRole("radio", { name: /Hollow Harbor/ }));
  if (folder) {
    await user.click(screen.getByRole("button", { name: "Choose folder…" }));
    await screen.findByText(/1.?204 files/);
  }
  if (label) await user.type(screen.getByRole("textbox", { name: /Version name/ }), label);
  if (key) await user.click(screen.getByRole("button", { name: "Choose key file…" }));
  if (passphrase) await user.type(screen.getByLabelText(/^Passphrase/), passphrase);
}

async function run(user: UserEvent, opts?: Parameters<typeof fill>[1]) {
  await fill(user, opts);
  await user.click(startButton());
}

describe("who can publish", () => {
  it("a player sees that only admins can publish and has no Publish entry", async () => {
    start({}, "user");
    expect(await screen.findByText(/Only admins can publish/)).toBeVisible();
    expect(screen.queryByRole("link", { name: "Publish" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Choose folder…" })).toBeNull();
  });

  it.each(["admin", "owner"] as const)("a%s sees the Publish entry and the form", async (role) => {
    start({}, role);
    expect(await screen.findByRole("button", { name: "Choose folder…" })).toBeVisible();
    expect(screen.getByRole("link", { name: "Publish" })).toBeVisible();
  });
});

describe("the plan preview", () => {
  it("shows files, size and packs, and offers the programs found", async () => {
    const { user } = start();
    await fill(user);
    expect(screen.getByText(/^1.?204 files · 5(\.0)? GB · 6 packs$/)).toBeVisible();
    expect(screen.getByText("Every file can be published.")).toBeVisible();
    await user.click(screen.getByRole("combobox", { name: "Program to start" }));
    expect(screen.getByRole("option", { name: "HollowHarbor.exe" })).toBeVisible();
    expect(screen.getByRole("option", { name: "tools/Editor.exe" })).toBeVisible();
  });

  it("invalid paths are listed with their reason and block publishing", async () => {
    const plan = {
      ...DEFAULT_PLAN,
      invalid: [
        { path: "Data/CON.txt", reason: "reserved_name" as const },
        { path: "a/b/Readme.md", reason: "case_collision" as const },
        { path: "link", reason: "symlink" as const },
      ],
      invalid_total: 503,
    };
    const { backend, user } = start({ plans: { [DEFAULT_PLAN.folder]: plan } });
    await run(user);
    expect(await screen.findByText("503 files can't be published")).toBeVisible();
    expect(screen.getByText("Data/CON.txt")).toBeVisible();
    expect(screen.getByText("Reserved name")).toBeVisible();
    expect(screen.getByText(/Same name as another file/)).toBeVisible();
    expect(screen.getByText("and 500 more")).toBeVisible();
    expect(startButton()).toHaveAttribute("aria-disabled", "true");
    expect(backend.callsTo("publish_start")).toHaveLength(0);
  });

  it.each([
    [{ kind: "not_a_folder" }, /isn't a folder/],
    [{ kind: "empty_folder" }, /folder is empty/],
    [{ kind: "too_many_files", limit: 1000000 }, /more than 1000000|more than 1,000,000/],
    [{ kind: "io", detail: "permission denied" }, /permission denied/],
  ] as const)("a failed scan (%j) says why", async (error, text) => {
    const { user } = start({ errors: { publish_plan: error } });
    await user.click(await screen.findByRole("button", { name: "Choose folder…" }));
    expect(await screen.findByText(text)).toBeVisible();
  });
});

describe("starting", () => {
  it("needs a game, a folder, a valid name, a key and a passphrase", async () => {
    const { backend, user } = start();
    await user.click(await screen.findByRole("radio", { name: /Hollow Harbor/ }));
    expect(startButton()).toHaveAttribute("aria-disabled", "true");
    await user.click(startButton());
    expect(backend.callsTo("publish_start")).toHaveLength(0);
  });

  it.each(["", "-x", "a b", "v".repeat(65), "naïve"])(
    "refuses the version name %j",
    async (label) => {
      const { backend, user } = start();
      await fill(user, { label });
      expect(startButton()).toHaveAttribute("aria-disabled", "true");
      expect(backend.callsTo("publish_start")).toHaveLength(0);
    },
  );

  it("a name the game already has is refused by the server", async () => {
    const { user } = start();
    await run(user, { label: "1.1.0" });
    expect(await screen.findByText(/already has a version with that name/)).toBeVisible();
  });

  it("a wrong passphrase is refused, nothing starts, and the field is emptied", async () => {
    const { backend, user } = start();
    await run(user, { passphrase: "nope" });
    expect(await screen.findByText("That passphrase doesn't unlock this key file.")).toBeVisible();
    expect(screen.getByLabelText(/^Passphrase/)).toHaveValue("");
    expect(screen.queryByRole("heading", { name: /^Publishing/ })).toBeNull();
    expect(backend.state.publish.jobs).toEqual({});
  });

  it("a key the server doesn't trust is explained and nothing is uploaded", async () => {
    const { backend, user } = start({ keyTrusted: false });
    await run(user);
    expect(await screen.findByText(/doesn't trust that key/)).toBeVisible();
    expect(backend.state.publish.jobs).toEqual({});
  });

  it("files that changed after the preview ask for a new preview", async () => {
    const { user } = start({ folderChanged: true });
    await run(user);
    expect(await screen.findByText(/changed after the check/)).toBeVisible();
  });

  it.each([
    [{ kind: "key_unreadable" }, /isn't a key file/],
    [{ kind: "forbidden" }, /Only admins can publish on this server/],
    [{ kind: "busy" }, /Another upload is already running/],
    [{ kind: "offline" }, /server can't be reached/],
    [{ kind: "io", detail: "disk error" }, /disk error/],
  ] as const)("a start error (%j) has its own message", async (error, text) => {
    const { user } = start({ errors: { publish_start: error } });
    await run(user);
    expect(await screen.findByText(text)).toBeVisible();
  });
});

describe("a publishing run", () => {
  it("uploads, verifies, and publishes only after a confirmation", async () => {
    const { backend, user } = start();
    await run(user);
    expect(await screen.findByRole("heading", { name: "Publishing 1.2.0" })).toBeVisible();
    expect(await screen.findByText("Verified. Ready to publish.")).toBeVisible();
    expect(backend.callsTo("publish_publish")).toHaveLength(0);
    // The start request carried exactly what the screen showed, and the passphrase went once.
    const request = backend.callsTo("publish_start")[0]?.args.request as Record<string, unknown>;
    expect(request).toMatchObject({
      package_id: backend.state.publish.packages[0]?.id,
      version_label: "1.2.0",
      platform: "windows-x86_64",
      executable: "HollowHarbor.exe",
      key_id: "key-1",
    });
    await user.click(screen.getByRole("button", { name: "Publish version" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Publish 1.2.0?" });
    expect(
      within(dialog).getByText(/Players will be offered 1.2.0 of Hollow Harbor/),
    ).toBeVisible();
    await user.click(within(dialog).getByRole("button", { name: "Publish" }));
    expect(await screen.findByText("Published.")).toBeVisible();
    expect(screen.getByText("1.2.0 of Hollow Harbor is now the current version.")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Publish another version" }));
    expect(await screen.findByRole("button", { name: "Choose folder…" })).toBeVisible();
    // The version list shows the new release as current.
    await user.click(await screen.findByRole("radio", { name: /Hollow Harbor/ }));
    const versions = await screen.findByRole("region", { name: "Versions of Hollow Harbor" });
    expect(await within(versions).findByText("1.2.0")).toBeVisible();
  });

  it("cancelling keeps the upload, and it continues with the passphrase", async () => {
    const { backend, user } = start({ auto: false });
    await run(user);
    await screen.findByRole("heading", { name: "Publishing 1.2.0" });
    const job = Object.keys(backend.state.publish.jobs)[0] as string;
    await act(async () => {
      await events.publishProgress.emit({
        job_id: job,
        package_id: backend.state.publish.packages[0]?.id as string,
        version_label: "1.2.0",
        phase: "uploading",
        bytes_done: 1024 ** 3,
        bytes_total: 5 * 1024 ** 3,
        packs: [{ index: 1, bytes_done: 50, bytes_total: 100 }],
        packs_done: 1,
        pack_count: 6,
        verification: null,
        failure: null,
      });
      await flush();
    });
    expect(screen.getByText("1 of 6 packs done")).toBeVisible();
    expect(screen.getByRole("progressbar", { name: "Pack 2" })).toHaveAttribute(
      "aria-valuenow",
      "50",
    );
    await user.click(screen.getByRole("button", { name: "Cancel upload" }));
    const confirm = await screen.findByRole("alertdialog", { name: "Cancel the upload?" });
    // "Keep uploading" does nothing.
    await user.click(within(confirm).getByRole("button", { name: "Keep uploading" }));
    expect(backend.callsTo("publish_cancel")).toHaveLength(0);
    await user.click(screen.getByRole("button", { name: "Cancel upload" }));
    await user.click(
      within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Cancel upload" }),
    );
    expect(await screen.findByText(/Cancelled\. The upload can continue later/)).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Continue upload" }));
    const dialog = await screen.findByRole("dialog", { name: "Continue the upload" });
    await user.type(within(dialog).getByLabelText(/^Passphrase/), "wrong");
    await user.click(within(dialog).getByRole("button", { name: "Continue upload" }));
    expect(
      await within(dialog).findByText("That passphrase doesn't unlock the key file."),
    ).toBeVisible();
    await user.clear(within(dialog).getByLabelText(/^Passphrase/));
    await user.type(within(dialog).getByLabelText(/^Passphrase/), CORRECT_PASSPHRASE);
    await user.click(within(dialog).getByRole("button", { name: "Continue upload" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(backend.callsTo("publish_resume")).toHaveLength(2);
    expect(screen.getByText("Uploading")).toBeVisible();
  });

  it("a failed verification can't be published or continued, only given up", async () => {
    const { user } = start({ scenario: "verification_failed" as PublishScenario });
    await run(user);
    expect(await screen.findByText(/data\/level3\.pak/)).toBeVisible();
    expect(screen.queryByRole("button", { name: "Publish version" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Continue upload" })).toBeNull();
    expect(screen.getByRole("button", { name: "Give up" })).toBeVisible();
  });

  it("a refused finish offers to continue, and giving up returns to the form", async () => {
    const { backend, user } = start({ scenario: "finalize_rejected" });
    await run(user);
    expect(await screen.findByText(/refused the upload \(pack_missing\)/)).toBeVisible();
    expect(screen.getByRole("button", { name: "Continue upload" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Give up" }));
    const confirm = await screen.findByRole("alertdialog", { name: "Give up on this version?" });
    await user.click(within(confirm).getByRole("button", { name: "Give up" }));
    expect(await screen.findByRole("button", { name: "Choose folder…" })).toBeVisible();
    expect(backend.callsTo("publish_abort")).toHaveLength(1);
  });

  it("losing the connection says so and offers to continue", async () => {
    const { user } = start({ scenario: "offline" });
    await run(user);
    expect(await screen.findByText(/can't be reached\. Continue when you're online/)).toBeVisible();
    expect(screen.getByRole("button", { name: "Continue upload" })).toBeVisible();
  });

  it("an upload left unfinished earlier is offered on the next visit", async () => {
    const { backend } = start();
    backend.state.publish.jobs.old = {
      job_id: "old",
      package_id: "p",
      version_label: "0.9.0",
      phase: "cancelled",
      bytes_done: 10,
      bytes_total: 100,
      packs: [],
      packs_done: 0,
      pack_count: 1,
      verification: null,
      failure: null,
      request: {} as never,
    };
    await act(async () => {
      await events.publishProgress.emit({
        ...backend.state.publish.jobs.old,
        phase: "cancelled",
      } as never);
    });
    expect(await screen.findByText(/Publishing 0\.9\.0 — Cancelled/)).toBeVisible();
  });
});

describe("games", () => {
  it("creates a game and selects it; a taken address is explained", async () => {
    const { user } = start();
    await user.type(
      await screen.findByRole("textbox", { name: "Title of the new game" }),
      "Paper Planet",
    );
    await user.click(screen.getByRole("button", { name: "Create game" }));
    expect(await screen.findByRole("radio", { name: /Paper Planet/ })).toBeChecked();
    await user.type(screen.getByRole("textbox", { name: "Title of the new game" }), "Paper Planet");
    await user.click(screen.getByRole("button", { name: "Create game" }));
    expect(await screen.findByText(/address "paper-planet" already exists/)).toBeVisible();
  });

  it("searches by title", async () => {
    const { user } = start();
    await user.type(await screen.findByRole("searchbox", { name: "Search your games" }), "crim");
    await waitFor(() => expect(screen.queryByRole("radio", { name: /Hollow Harbor/ })).toBeNull());
    expect(await screen.findByRole("radio", { name: /Crimson Canyon/ })).toBeVisible();
  });
});

describe("withdrawing a version", () => {
  async function openYank(user: UserEvent) {
    await user.click(await screen.findByRole("radio", { name: /Hollow Harbor/ }));
    await user.click(await screen.findByRole("button", { name: "Withdraw 1.0.0" }));
    return screen.findByRole("dialog", { name: "Withdraw 1.0.0?" });
  }

  it("needs a reason of 3 to 500 characters, then shows the server's result", async () => {
    const { backend, user } = start();
    const dialog = await openYank(user);
    const reason = within(dialog).getByRole("textbox", { name: /^Reason/ });
    await user.type(reason, "ab");
    await user.click(within(dialog).getByRole("button", { name: "Withdraw version" }));
    expect(await within(dialog).findByText(/reason of 3 to 500 characters/)).toBeVisible();
    expect(backend.callsTo("publish_yank")).toHaveLength(0);
    await user.clear(reason);
    await user.type(reason, "Crashes on start");
    await user.click(within(dialog).getByRole("button", { name: "Withdraw version" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(backend.state.publish.yanked[0]?.reason).toBe("Crashes on start");
    const versions = screen.getByRole("region", { name: "Versions of Hollow Harbor" });
    expect(await within(versions).findByText("Withdrawn")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Withdraw 1.0.0" })).toBeNull();
  });

  it("does not assume success: the server's refusal stays in the dialog", async () => {
    const { user } = start({ errors: { publish_yank: { kind: "forbidden" } } });
    const dialog = await openYank(user);
    await user.type(within(dialog).getByRole("textbox", { name: /^Reason/ }), "Crashes on start");
    await user.click(within(dialog).getByRole("button", { name: "Withdraw version" }));
    expect(await within(dialog).findByText("Only admins can withdraw versions.")).toBeVisible();
    expect(screen.getByRole("button", { name: "Withdraw 1.0.0" })).toBeVisible();
  });
});
