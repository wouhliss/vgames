import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { routes } from "../../app/router";
import type {
  DownloadError,
  DownloadHistoryEntry,
  DownloadJob,
  InstallPhase,
  PauseReason,
} from "../../ipc";
import { events } from "../../ipc";
import { installMockBackend, MOCK_LIBRARY, MOCK_SERVER, type MockState } from "../../mocks/backend";
import { makeCatalog } from "../../mocks/catalog";
import { makeJob } from "../../mocks/downloads";
import { flush, renderWithProviders } from "../../test/render";

const GiB = 1024 ** 3;
const CATALOG = makeCatalog(8);

function job(i: number, title: string, patch: Partial<DownloadJob> = {}): DownloadJob {
  const pkg = CATALOG[i];
  if (!pkg) throw new Error("fixture");
  return makeJob({ ...pkg, title }, MOCK_SERVER.id, MOCK_LIBRARY.id, {
    version_label: "1.2.0",
    bytes_total: 10 * GiB,
    ...patch,
  });
}

const HARBOR = job(0, "Hollow Harbor", { state: { kind: "active" }, bytes_done: 1 * GiB });
const CANYON = job(1, "Crimson Canyon");
const STATION = job(2, "Silent Station", { bytes_done: 2 * GiB });
const GARDEN = job(3, "Gilded Garden");

const paused = (reason: PauseReason) => ({ kind: "paused" as const, reason });
const failed = (error: DownloadError) => ({ kind: "failed" as const, error });

type Backend = ReturnType<typeof installMockBackend>;

function setup(overrides: Partial<MockState> = {}, configure?: (backend: Backend) => void) {
  const backend = installMockBackend({
    servers: [MOCK_SERVER],
    libraries: [MOCK_LIBRARY],
    downloads: [HARBOR, CANYON, STATION, GARDEN],
    downloadHistory: [],
    ...overrides,
  });
  configure?.(backend);
  const router = createMemoryRouter(routes, { initialEntries: ["/downloads"] });
  renderWithProviders(<RouterProvider router={router} />);
  return { backend, router, user: userEvent.setup() };
}

const row = (title: string) => {
  const heading = screen.getByRole("heading", { level: 3, name: title });
  const li = heading.closest("li");
  if (!li) throw new Error(`no row for ${title}`);
  return li;
};

async function progress(
  target: DownloadJob,
  phase: InstallPhase,
  done: number,
  extra: { rate?: number; eta?: number | null; connections?: number } = {},
) {
  await act(async () => {
    await events.installProgress.emit({
      package: target.package,
      phase,
      bytes_done: done,
      bytes_total: target.bytes_total,
      bytes_per_second: extra.rate ?? 0,
      eta_seconds: extra.eta ?? null,
      connections: extra.connections ?? 0,
    });
    await flush();
  });
}

beforeEach(() => {
  localStorage.clear();
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(1000);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(800);
});

describe("downloads", () => {
  it("shows an empty state with a way to the catalog", async () => {
    const { user, router } = setup({ downloads: [] });
    expect(await screen.findByRole("heading", { name: "Nothing is downloading" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Browse the catalog" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/browse"));
  });

  it("splits the queue into running and up next, in order", async () => {
    setup();
    const running = await screen.findByRole("region", { name: "Downloading" });
    expect(
      within(running)
        .getAllByRole("heading", { level: 3 })
        .map((h) => h.textContent),
    ).toEqual(["Hollow Harbor"]);
    const next = screen.getByRole("region", { name: "Up next" });
    expect(
      within(next)
        .getAllByRole("heading", { level: 3 })
        .map((h) => h.textContent),
    ).toEqual(["Crimson Canyon", "Silent Station", "Gilded Garden"]);
    expect(row("Silent Station")).toHaveTextContent("Waiting for its turn · 2.0 GB of 10.0 GB");
    expect(row("Hollow Harbor")).toHaveTextContent("Version 1.2.0");
  });

  describe("phases", () => {
    it.each([
      ["verifying_manifest", "Verifying the signature"],
      ["allocating", "Making room on the drive"],
      ["finalizing", "Finishing"],
    ] as const)("%s shows an indeterminate bar: %s", async (phase, text) => {
      setup();
      await screen.findByRole("region", { name: "Downloading" });
      await progress(HARBOR, phase, 0);
      const item = row("Hollow Harbor");
      expect(within(item).getByText(text)).toBeVisible();
      expect(within(item).getByRole("progressbar")).not.toHaveAttribute("aria-valuenow");
    });

    it("downloading shows bytes, speed, time left and connections", async () => {
      setup();
      await screen.findByRole("region", { name: "Downloading" });
      await progress(HARBOR, "downloading", 3.7 * GiB, {
        rate: 48 * 1024 ** 2,
        eta: 131,
        connections: 12,
      });
      const item = row("Hollow Harbor");
      expect(item).toHaveTextContent(
        "Downloading · 3.7 GB of 10.0 GB · 48.0 MB/s · 2 minutes left · 12 connections",
      );
      const bar = within(item).getByRole("progressbar", { name: "Progress of Hollow Harbor" });
      expect(bar).toHaveAttribute("aria-valuenow", "37");
      expect(bar).toHaveAttribute("aria-valuetext", "3.7 GB of 10.0 GB");
    });

    it("verifying (a repair) shows how much was checked", async () => {
      const repair = { ...HARBOR, kind: "repair" as const };
      setup({ downloads: [repair] });
      await screen.findByRole("region", { name: "Downloading" });
      await progress(repair, "verifying", 5 * GiB);
      const item = row("Hollow Harbor");
      expect(item).toHaveTextContent("Repair of version 1.2.0");
      expect(item).toHaveTextContent("Checking files · 5.0 GB of 10.0 GB");
      expect(within(item).getByRole("progressbar")).toHaveAttribute("aria-valuenow", "50");
    });

    it("a finished job leaves the queue and shows up in the completed list", async () => {
      const { backend } = setup();
      await screen.findByRole("region", { name: "Downloading" });
      await progress(HARBOR, "downloading", 9 * GiB, { rate: 1e8, eta: 10, connections: 6 });
      const entry: DownloadHistoryEntry = {
        id: "h1",
        package: HARBOR.package,
        title: HARBOR.title,
        kind: "install",
        version_label: "1.2.0",
        bytes_total: HARBOR.bytes_total,
        finished_at: new Date().toISOString(),
        outcome: { kind: "installed" },
      };
      backend.state.downloads = backend.state.downloads.filter((j) => j !== HARBOR);
      backend.state.downloadHistory = [entry];
      await act(async () => {
        await events.installFinished.emit({
          package: HARBOR.package,
          outcome: { kind: "installed" },
        });
        await flush();
      });
      const done = await screen.findByRole("region", { name: "Completed" });
      expect(within(done).getByText("Hollow Harbor")).toBeVisible();
      expect(within(done).getByText("Installed version 1.2.0")).toBeVisible();
      expect(screen.queryByRole("heading", { level: 3, name: "Hollow Harbor" })).toBeNull();
    });
  });

  describe("paused", () => {
    it("by the player: Resume queues it again", async () => {
      const { user, backend } = setup({
        downloads: [HARBOR, { ...CANYON, state: paused({ kind: "user" }) }],
      });
      const item = await waitFor(() => row("Crimson Canyon"));
      expect(item).toHaveTextContent("Paused · 0 B of 10.0 GB");
      await user.click(within(item).getByRole("button", { name: "Resume Crimson Canyon" }));
      expect(backend.callsTo("download_resume")[0]?.args).toEqual({ package: CANYON.package });
      await waitFor(() => expect(row("Crimson Canyon")).toHaveTextContent("Waiting for its turn"));
    });

    it("the disk is full: says how much is missing and links to Storage settings", async () => {
      const { user, router } = setup({
        downloads: [
          {
            ...HARBOR,
            state: paused({
              kind: "disk_full",
              library_path: "/home/sam/Games",
              required_bytes: 4 * GiB,
              available_bytes: 1.5 * GiB,
            }),
          },
        ],
      });
      const item = await waitFor(() => row("Hollow Harbor"));
      expect(within(item).getByRole("status")).toHaveTextContent(
        "Not enough space on /home/sam/Games: 4.0 GB needed, 1.5 GB free. It continues by itself when there's room.",
      );
      await user.click(within(item).getByRole("button", { name: "Open Storage settings" }));
      await waitFor(() => expect(router.state.location.pathname).toBe("/settings/storage"));
    });

    it("the drive is disconnected, or the server is unreachable: it continues by itself", async () => {
      setup({
        downloads: [
          { ...HARBOR, state: paused({ kind: "library_offline", library_path: "/media/sam/Ext" }) },
          { ...CANYON, state: paused({ kind: "offline" }) },
        ],
      });
      expect(await waitFor(() => row("Hollow Harbor"))).toHaveTextContent(
        "The drive /media/sam/Ext isn't connected. It continues when you connect it again.",
      );
      expect(row("Crimson Canyon")).toHaveTextContent(
        "Waiting for the server. It continues by itself when the connection is back.",
      );
    });

    it("pausing the running job", async () => {
      const { user, backend } = setup();
      const item = await waitFor(() => row("Hollow Harbor"));
      await user.click(within(item).getByRole("button", { name: "Pause Hollow Harbor" }));
      expect(backend.callsTo("download_pause")).toHaveLength(1);
      // The next job starts; Hollow Harbor waits, paused.
      await waitFor(() =>
        expect(
          within(screen.getByRole("region", { name: "Downloading" })).getByText("Crimson Canyon"),
        ).toBeVisible(),
      );
      expect(
        within(row("Hollow Harbor")).getByRole("button", { name: "Resume Hollow Harbor" }),
      ).toBeVisible();
    });
  });

  describe("failed", () => {
    it.each([
      [true, "It told the server's admins, so try again after they fix it."],
      [false, "Tell the server's admins so they can fix it."],
    ])("damaged server file (reported: %s)", async (reported, text) => {
      const { user, backend } = setup({
        downloads: [{ ...HARBOR, state: failed({ kind: "damaged_file", reported }) }],
      });
      const item = await waitFor(() => row("Hollow Harbor"));
      const alert = within(item).getByRole("alert");
      expect(alert).toHaveTextContent("The server has a damaged file");
      expect(alert).toHaveTextContent(text);
      await user.click(within(item).getByRole("button", { name: "Try Hollow Harbor again" }));
      expect(backend.callsTo("download_retry")).toHaveLength(1);
    });

    it.each([
      ["signature_invalid", "isn't signed the way the server's publishers sign games"],
      ["untrusted_key", "signed with a key the server doesn't trust anymore"],
      ["trust_expired", "The server's list of trusted publishers has expired"],
    ] as const)("%s: a security explanation and no retry", async (kind, text) => {
      const { user, backend } = setup({ downloads: [{ ...HARBOR, state: failed({ kind }) }] });
      const item = await waitFor(() => row("Hollow Harbor"));
      const alert = within(item).getByRole("alert");
      expect(alert).toHaveTextContent("Stopped to keep your computer safe");
      expect(alert).toHaveTextContent(text);
      expect(within(item).queryByRole("button", { name: /again/ })).toBeNull();
      expect(within(item).queryByRole("progressbar")).toBeNull();
      await user.click(
        within(item).getByRole("button", { name: "Remove Hollow Harbor from the list" }),
      );
      expect(backend.callsTo("download_remove")).toHaveLength(1);
      expect(backend.callsTo("download_retry")).toHaveLength(0);
      await waitFor(() => expect(screen.queryByRole("heading", { level: 3 })).toBeNull());
    });

    it.each([
      [
        { kind: "io", path: "/mnt/games/x", detail: "Permission denied" },
        "Couldn't write the files",
        "/mnt/games/x: Permission denied",
        true,
      ],
      [
        { kind: "server", code: "internal", message: "boom" },
        "The server had a problem",
        "It answered with an error (internal).",
        true,
      ],
      [
        { kind: "version_unavailable" },
        "This version isn't available anymore",
        "withdrew it",
        false,
      ],
    ] as const)("%o", async (error, title, text, retry) => {
      setup({ downloads: [{ ...HARBOR, state: failed(error) }] });
      const item = await waitFor(() => row("Hollow Harbor"));
      const alert = within(item).getByRole("alert");
      expect(alert).toHaveTextContent(title);
      expect(alert).toHaveTextContent(text);
      expect(Boolean(within(item).queryByRole("button", { name: "Try Hollow Harbor again" }))).toBe(
        retry,
      );
    });
  });

  describe("cancel", () => {
    it("keeps the downloaded files by default", async () => {
      const { user, backend } = setup();
      const item = await waitFor(() => row("Silent Station"));
      await user.click(within(item).getByRole("button", { name: "Cancel Silent Station" }));
      const dialog = screen.getByRole("dialog", { name: "Cancel installing Silent Station?" });
      expect(
        within(dialog).getByRole("radio", { name: /Keep the downloaded files/ }),
      ).toBeChecked();
      expect(
        within(dialog).getByRole("radio", { name: /Delete the downloaded files/ }),
      ).toHaveTextContent("Frees 2.0 GB.");
      await user.click(within(dialog).getByRole("button", { name: "Cancel download" }));
      expect(backend.callsTo("download_cancel")[0]?.args).toEqual({
        package: STATION.package,
        keepPartial: true,
      });
      expect(await screen.findByText("Cancelled, files kept")).toBeVisible();
      expect(screen.queryByRole("dialog")).toBeNull();
    });

    it("can delete them instead; an update keeps the installed version", async () => {
      const update = { ...CANYON, kind: "update" as const, bytes_done: GiB };
      const { user, backend } = setup({ downloads: [HARBOR, update] });
      const item = await waitFor(() => row("Crimson Canyon"));
      await user.click(within(item).getByRole("button", { name: "Cancel Crimson Canyon" }));
      const dialog = screen.getByRole("dialog", { name: "Cancel the update of Crimson Canyon?" });
      expect(dialog).toHaveTextContent("The installed version stays as it is.");
      await user.click(within(dialog).getByRole("radio", { name: /Delete the downloaded files/ }));
      await user.click(within(dialog).getByRole("button", { name: "Cancel download" }));
      expect(backend.callsTo("download_cancel")[0]?.args).toEqual({
        package: CANYON.package,
        keepPartial: false,
      });
      expect(await screen.findByText("Cancelled")).toBeVisible();
    });

    it("asks nothing more when nothing was downloaded yet", async () => {
      const { user, backend } = setup();
      const item = await waitFor(() => row("Gilded Garden"));
      await user.click(within(item).getByRole("button", { name: "Cancel Gilded Garden" }));
      const dialog = screen.getByRole("dialog", { name: "Cancel installing Gilded Garden?" });
      expect(dialog).toHaveTextContent("Nothing has been downloaded yet.");
      expect(within(dialog).queryByRole("radio")).toBeNull();
      await user.click(within(dialog).getByRole("button", { name: "Cancel download" }));
      expect(backend.callsTo("download_cancel")[0]?.args).toEqual({
        package: GARDEN.package,
        keepPartial: false,
      });
    });

    it("Keep downloading closes the dialog and changes nothing", async () => {
      const { user, backend } = setup();
      await user.click(
        within(await waitFor(() => row("Hollow Harbor"))).getByRole("button", {
          name: "Cancel Hollow Harbor",
        }),
      );
      await user.click(screen.getByRole("button", { name: "Keep downloading" }));
      expect(screen.queryByRole("dialog")).toBeNull();
      expect(backend.callsTo("download_cancel")).toHaveLength(0);
    });
  });

  describe("reorder", () => {
    const names = () =>
      within(screen.getByRole("region", { name: "Up next" }))
        .getAllByRole("heading", { level: 3 })
        .map((h) => h.textContent);

    it("moves a job to the front from its menu", async () => {
      const { user, backend } = setup();
      const item = await waitFor(() => row("Gilded Garden"));
      await user.click(
        within(item).getByRole("button", { name: "More actions for Gilded Garden" }),
      );
      await user.click(screen.getByRole("menuitem", { name: "Download next" }));
      expect(backend.callsTo("downloads_reorder")[0]?.args).toEqual({
        packages: [GARDEN.package, CANYON.package, STATION.package],
      });
      await waitFor(() =>
        expect(names()).toEqual(["Gilded Garden", "Crimson Canyon", "Silent Station"]),
      );
    });

    it("Alt+arrows move the focused job, announce it and keep focus on it", async () => {
      const { user } = setup();
      const item = await waitFor(() => row("Crimson Canyon"));
      const more = within(item).getByRole("button", { name: "More actions for Crimson Canyon" });
      more.focus();
      await user.keyboard("{Alt>}{ArrowDown}{/Alt}");
      await waitFor(() =>
        expect(names()).toEqual(["Silent Station", "Crimson Canyon", "Gilded Garden"]),
      );
      expect(screen.getByText("Crimson Canyon moved to position 2")).toBeInTheDocument();
      await waitFor(() =>
        expect(
          within(row("Crimson Canyon")).getByRole("button", {
            name: "More actions for Crimson Canyon",
          }),
        ).toHaveFocus(),
      );
    });

    it("the first job can't move up and the last can't move down", async () => {
      const { user } = setup();
      const first = await waitFor(() => row("Crimson Canyon"));
      await user.click(
        within(first).getByRole("button", { name: "More actions for Crimson Canyon" }),
      );
      expect(screen.getByRole("menuitem", { name: /Move up/ })).toHaveAttribute(
        "aria-disabled",
        "true",
      );
      expect(screen.getByRole("menuitem", { name: /Download next/ })).toHaveAttribute(
        "aria-disabled",
        "true",
      );
      await user.keyboard("{Escape}");
      const last = row("Gilded Garden");
      await user.click(
        within(last).getByRole("button", { name: "More actions for Gilded Garden" }),
      );
      expect(screen.getByRole("menuitem", { name: /Move down/ })).toHaveAttribute(
        "aria-disabled",
        "true",
      );
    });
  });

  it("clears the completed list", async () => {
    const entry: DownloadHistoryEntry = {
      id: "h1",
      package: GARDEN.package,
      title: "Gilded Garden",
      kind: "update",
      version_label: "2.0.0",
      bytes_total: GiB,
      finished_at: new Date().toISOString(),
      outcome: { kind: "failed", code: "io", message: "Permission denied" },
    };
    const { user } = setup({ downloads: [HARBOR], downloadHistory: [entry] });
    const done = await screen.findByRole("region", { name: "Completed" });
    expect(done).toHaveTextContent("Failed: Permission denied");
    await user.click(within(done).getByRole("button", { name: "Clear the completed list" }));
    expect(await screen.findByText("Completed list cleared")).toBeVisible();
    await waitFor(() => expect(screen.queryByRole("region", { name: "Completed" })).toBeNull());
  });

  it("says why an action failed", async () => {
    const { user, backend } = setup({
      downloads: [HARBOR, { ...CANYON, state: paused({ kind: "user" }) }],
    });
    backend.on("download_resume", () =>
      Promise.reject({ kind: "insufficient_space", required_bytes: 4 * GiB, available_bytes: GiB }),
    );
    await user.click(
      within(await waitFor(() => row("Crimson Canyon"))).getByRole("button", {
        name: "Resume Crimson Canyon",
      }),
    );
    expect(await screen.findByText("Not enough space: 4.0 GB needed, 1.0 GB free.")).toBeVisible();
  });

  it("shows an error with a retry when the queue can't be read", async () => {
    let failing = true;
    const { user } = setup({}, (backend) => {
      backend.on("downloads_list", () =>
        failing
          ? Promise.reject({ kind: "internal", detail: "db" })
          : { jobs: [HARBOR], history: [] },
      );
    });
    expect(
      await screen.findByRole("heading", { name: "Couldn't load your downloads" }),
    ).toBeVisible();
    failing = false;
    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("heading", { level: 3, name: "Hollow Harbor" })).toBeVisible();
  });
});
