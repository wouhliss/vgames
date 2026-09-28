import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { packageId, versionId } from "../../mocks/db";
import { MOCK_PASSPHRASE, mockKeyFile } from "../../mocks/keyfile";
import { renderAt } from "../../test/render";
import { inProcessWorkers } from "../../test/workers";
import { UploadDepsContext } from "../../upload/context";
import { gcsFetchOptions } from "../../upload/gcs";
import { memoryStore, type UploadStore } from "../../upload/store";

const HARBOR = packageId(1);
const MiB = 1024 * 1024;
let store: UploadStore;

function wrapper({ children }: { children: ReactNode }) {
  return (
    <UploadDepsContext.Provider value={{ workers: inProcessWorkers(8 * MiB), store: () => store }}>
      {children}
    </UploadDepsContext.Provider>
  );
}

/** Files as `<input webkitdirectory>` gives them: paths include the picked folder's name. */
function folderFiles(entries: [string, number][]): File[] {
  return entries.map(([path, size], i) => {
    const file = new File([new Uint8Array(size).fill(i + 1)], path.split("/").pop() ?? path, {
      lastModified: 1_700_000_000_000 + i,
    });
    Object.defineProperty(file, "webkitRelativePath", { value: `MyGame/${path}` });
    return file;
  });
}

const GOOD: [string, number][] = [
  ["bin/game.exe", 9 * MiB],
  ["data/level1.pak", 5 * MiB],
  ["readme.txt", 1000],
];

function pick(files: File[]) {
  fireEvent.change(screen.getByLabelText("Game folder"), { target: { files } });
}

async function created(platform: string) {
  const step = screen.getByRole("region", { name: "1. Version" });
  await waitFor(() => expect(step).toHaveTextContent(`for ${platform}`));
}

function keyFile(content = mockKeyFile()) {
  return new File([content], "adrian.vgkey");
}

beforeEach(() => {
  store = memoryStore();
  gcsFetchOptions.redirect = "manual";
  // jsdom has no layout: give the virtualized lists a size.
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(280);
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(600);
});
afterEach(() => {
  gcsFetchOptions.redirect = "follow";
  vi.restoreAllMocks();
});

describe("versions tab", () => {
  it("lists versions with state, size, creator and the current-release badge", async () => {
    renderAt(`/packages/${HARBOR}/versions`);
    const table = await screen.findByRole("table", { name: "Versions, newest first" });
    const rows = within(table).getAllByRole("row");
    expect(rows[1]).toHaveTextContent("31.2.0linux-x86_64Ready to publish1.2 GB1,432 filesAdrian");
    expect(rows[2]).toHaveTextContent("1.1.0 Current release");
    expect(rows[3]).toHaveTextContent("Withdrawn");
  });

  it("publishes a ready version after confirmation", async () => {
    const { user, db } = renderAt(`/packages/${HARBOR}/versions`);
    await user.click(await screen.findByRole("button", { name: "Publish…" }));
    const dialog = await screen.findByRole("dialog", { name: "Publish 1.2.0?" });
    await user.click(within(dialog).getByRole("button", { name: "Publish" }));
    expect(await screen.findByText("Published 1.2.0.")).toBeInTheDocument();
    expect(db.versions[HARBOR]?.find((v) => v.id === versionId(3))?.state).toBe("published");
    expect(await screen.findByRole("row", { name: /1\.2\.0 Current release/ })).toBeInTheDocument();
  });

  it("yanks only with a reason of 3 to 500 characters", async () => {
    const { user, db } = renderAt(`/packages/${HARBOR}/versions`);
    await user.click(await screen.findByRole("button", { name: "Yank…" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Yank 1.1.0?" });
    await user.type(within(dialog).getByLabelText("Reason (required)"), "no");
    await user.click(within(dialog).getByRole("button", { name: "Yank version" }));
    expect(within(dialog).getByText(/Give a reason of 3 to 500 characters/)).toBeInTheDocument();
    await user.type(within(dialog).getByLabelText("Reason (required)"), "t start on Windows 10");
    await user.click(within(dialog).getByRole("button", { name: "Yank version" }));
    expect(await screen.findByText("Withdrew 1.1.0.")).toBeInTheDocument();
    expect(db.versions[HARBOR]?.find((v) => v.id === versionId(2))?.state).toBe("yanked");
  });

  it("aborts an unpublished version after confirmation", async () => {
    const { user, db } = renderAt(`/packages/${HARBOR}/versions`);
    await user.click(await screen.findByRole("button", { name: "Abort…" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Abort 1.2.0?" });
    expect(within(dialog).getByRole("button", { name: "Keep it" })).toHaveFocus();
    await user.click(within(dialog).getByRole("button", { name: "Abort version" }));
    expect(await screen.findByText("Aborted 1.2.0.")).toBeInTheDocument();
    expect(db.versions[HARBOR]?.find((v) => v.id === versionId(3))?.state).toBe("aborted");
  });

  it("shows verification progress while a version verifies", async () => {
    renderAt(`/packages/${HARBOR}/versions`, "admin", {
      db: (db) => {
        const list = db.versions[HARBOR] ?? [];
        db.versions[HARBOR] = list.map((v) =>
          v.id === versionId(3) ? { ...v, state: "verifying", verify_progress: 0 } : v,
        );
      },
    });
    expect(await screen.findByRole("progressbar", { name: "Verifying 1.2.0" })).toBeInTheDocument();
    // Polled every 2 s until it's done.
    expect(
      await screen.findByText("Ready to publish", {}, { timeout: 12_000 }),
    ).toBeInTheDocument();
  }, 15_000);
});

describe("upload wizard", () => {
  it("uploads a folder end to end: version, folder, launch, key, upload, verify, publish", async () => {
    const { user, db } = renderAt(`/packages/${HARBOR}/versions/new`, "admin", { wrapper });
    await user.selectOptions(await screen.findByLabelText("Platform"), "linux-x86_64");
    await user.click(screen.getByRole("button", { name: "Create version" }));
    expect(await screen.findByText(/A version label has 1 to 64 characters/)).toBeInTheDocument();
    await user.type(screen.getByLabelText("Version label"), "2.0.0");
    await user.click(screen.getByRole("button", { name: "Create version" }));
    await created("linux-x86_64");

    pick(folderFiles(GOOD));
    expect(await screen.findByText(/3 files, .*, 2 packs/)).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Files to upload" })).toHaveTextContent(
      "bin/game.exe",
    );

    await user.type(screen.getByLabelText("Executable (path in the folder)"), "bin/game.exe");
    await user.upload(screen.getByLabelText("Publisher key file"), keyFile());
    await user.type(screen.getByLabelText("Passphrase"), "wrong");
    await user.click(screen.getByRole("button", { name: "Unlock key" }));
    expect(await screen.findByText("The passphrase is wrong.")).toBeInTheDocument();
    expect(screen.getByLabelText("Passphrase")).toHaveAttribute("aria-invalid", "true");
    await user.clear(screen.getByLabelText("Passphrase"));
    await user.type(screen.getByLabelText("Passphrase"), MOCK_PASSPHRASE);
    await user.click(screen.getByRole("button", { name: "Unlock key" }));
    expect(await screen.findByText(/Key ready: Adrian's laptop/)).toBeInTheDocument();
    // The passphrase field is gone; nothing kept it.
    expect(screen.queryByLabelText("Passphrase")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Start upload" }));
    expect(
      await screen.findByText(
        "Verified. The version is ready to publish.",
        {},
        { timeout: 15_000 },
      ),
    ).toBeInTheDocument();
    const bar = screen.getByRole("progressbar", { name: "Upload progress" }) as HTMLProgressElement;
    expect(bar.value).toBe(bar.max);
    expect(bar.max).toBeGreaterThan(14 * MiB);
    await user.click(screen.getByRole("button", { name: "Publish 2.0.0" }));
    expect(await screen.findByText(/Published 2\.0\.0/)).toBeInTheDocument();
    const made = db.versions[HARBOR]?.find((v) => v.version_label === "2.0.0");
    expect(made?.state).toBe("published");
    expect(await store.list()).toEqual([]);
  }, 30_000);

  it("lists invalid paths and blocks the upload", async () => {
    const { user } = renderAt(`/packages/${HARBOR}/versions/new`, "admin", { wrapper });
    await user.type(await screen.findByLabelText("Version label"), "2.0.1");
    await user.click(screen.getByRole("button", { name: "Create version" }));
    await created("windows-x86_64");
    pick(folderFiles([...GOOD, ["aux.txt", 5], ["bad:name.txt", 5], ["Readme.TXT", 5]]));
    const list = await screen.findByRole("region", { name: "Invalid paths" });
    expect(screen.getByText("3 paths are not allowed")).toBeInTheDocument();
    expect(list).toHaveTextContent('aux.txt: "aux.txt" is a reserved name on Windows');
    expect(list).toHaveTextContent("bad:name.txt");
    expect(list).toHaveTextContent('Readme.TXT: same name as "readme.txt" apart from letter case');
    expect(screen.getByRole("region", { name: "3. What the launcher starts" })).toHaveTextContent(
      "Complete the step above first.",
    );
  });

  it("refuses a root key file", async () => {
    const { user } = renderAt(`/packages/${HARBOR}/versions/new`, "admin", { wrapper });
    await user.type(await screen.findByLabelText("Version label"), "2.0.2");
    await user.click(screen.getByRole("button", { name: "Create version" }));
    await created("windows-x86_64");
    pick(folderFiles(GOOD));
    await screen.findByRole("region", { name: "Files to upload" });
    await user.click(screen.getByLabelText(/Nothing to start/));
    await user.upload(
      screen.getByLabelText("Publisher key file"),
      keyFile(mockKeyFile({ kind: "root" })),
    );
    await user.type(screen.getByLabelText("Passphrase"), MOCK_PASSPHRASE);
    await user.click(screen.getByRole("button", { name: "Unlock key" }));
    expect(await screen.findByText(/This is the server's root key/)).toBeInTheDocument();
  });

  it("on resume, refuses a folder that changed since the upload started", async () => {
    await store.put({
      versionId: versionId(9),
      packageId: HARBOR,
      serverId: "01920000-0000-7000-8000-000000000001",
      platform: "linux-x86_64",
      label: "3.0",
      sequence: 4,
      createdAt: 1_790_000_000,
      files: [
        { path: "bin/game.exe", size: 9 * MiB, mtime_ms: 1_700_000_000_000 },
        { path: "data/level1.pak", size: 5 * MiB, mtime_ms: 1_700_000_000_001 },
        { path: "readme.txt", size: 999, mtime_ms: 1_700_000_000_002 },
      ],
      directories: [],
      packs: [8 * MiB, 6 * MiB],
      execution: undefined,
      sessions: {},
      confirmed: { 0: 1024 },
      complete: [],
      savedAt: new Date().toISOString(),
    });
    renderAt(`/packages/${HARBOR}/versions/${versionId(9)}/upload`, "admin", {
      wrapper,
      db: (db) => {
        db.versions[HARBOR] = [
          {
            id: versionId(9),
            package_id: HARBOR,
            server_id: "01920000-0000-7000-8000-000000000001",
            platform: "linux-x86_64",
            sequence: 4,
            version_label: "3.0",
            state: "uploading",
            created_at: "2026-09-28T10:00:00Z",
            created_by: { id: "01920000-0000-7000-8000-00000000a002", username: "adrian" },
          },
          ...(db.versions[HARBOR] ?? []),
        ];
      },
    });
    expect(await screen.findByText(/Pick the same folder again to continue/)).toBeInTheDocument();
    pick(folderFiles(GOOD));
    expect(
      await screen.findByText("This isn't the folder the upload started with"),
    ).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Differences" })).toHaveTextContent(
      "readme.txt changed size",
    );
  });

  it("says when a version is no longer uploading", async () => {
    renderAt(`/packages/${HARBOR}/versions/${versionId(3)}/upload`, "admin", { wrapper });
    expect(await screen.findByText(/no longer uploading \(it is ready\)/)).toBeInTheDocument();
  });
});
