import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { routes } from "../../app/router";
import type { InstalledPackage, LibraryInfo } from "../../ipc";
import { installMockBackend, MOCK_LIBRARY, MOCK_SERVER, type MockState } from "../../mocks/backend";
import { catalogHandlers, type MockPackage, makeCatalog } from "../../mocks/catalog";
import { makeInstalls, OFFLINE_LIBRARY } from "../../mocks/library";
import { renderWithProviders } from "../../test/render";

const GiB = 1024 ** 3;

function pkg(i: number, title: string, patch: Partial<MockPackage> = {}): MockPackage {
  const base = makeCatalog(i + 1)[i];
  if (!base) throw new Error("fixture");
  return { ...base, title, slug: title.toLowerCase().replace(/\s+/g, "-"), ...patch };
}

const HARBOR = pkg(0, "Hollow Harbor", {
  summary: "A cosy harbour town.",
  description: '**Hollow Harbor** is calm.\n\n<img src=x onerror="alert(1)">',
  developer: "Lantern Works",
  publisher: null,
  platforms: ["linux-x86_64", "windows-x86_64", "macos-aarch64"],
  total_size: 12 * GiB,
  version_label: "1.4.2",
});
const CANYON = pkg(1, "Crimson Canyon", {
  platforms: ["windows-x86_64"],
  profile: { status: "playable", notes: "Cutscenes stutter on first play. <b>not bold</b>" },
  protondb: "gold",
});
const STATION = pkg(2, "Silent Station", { platforms: ["macos-aarch64"] });
const GARDEN = pkg(3, "Gilded Garden", { platforms: ["windows-x86_64"], d3d12: true });
const FORGE = pkg(4, "Frozen Forge", { platforms: ["macos-x86_64"] });
const ENGINE = pkg(5, "Electric Engine", {
  platforms: ["linux-x86_64"],
  total_size: 500 * GiB,
  screenshots: [],
});

const SSD: LibraryInfo = {
  ...MOCK_LIBRARY,
  id: "01920000-0000-7000-8000-0000000000b3",
  path: "/mnt/ssd",
  label: "Fast SSD",
  is_default: false,
  free_bytes: 900 * GiB,
};

function installed(target: MockPackage, patch: Partial<InstalledPackage> = {}): InstalledPackage {
  const [base] = makeInstalls(1, MOCK_SERVER, [MOCK_LIBRARY]);
  if (!base) throw new Error("fixture");
  return {
    ...base,
    title: target.title,
    slug: target.slug,
    package: { ...base.package, package_id: target.id },
    state: "installed",
    running: false,
    update: null,
    targets: [{ id: "play", label: "Play", is_default: true }],
    compat: "native",
    cloud_saves: "unsupported",
    ...patch,
  };
}

function setup(target: MockPackage, overrides: Partial<MockState> = {}) {
  const backend = installMockBackend({
    servers: [MOCK_SERVER],
    libraries: [MOCK_LIBRARY, OFFLINE_LIBRARY],
    installs: [],
    packages: [HARBOR, CANYON, STATION, GARDEN, FORGE, ENGINE],
    actionDelayMs: 0,
    ...overrides,
  });
  const router = createMemoryRouter(routes, {
    initialEntries: ["/browse", `/package/${target.id}`],
    initialIndex: 1,
  });
  renderWithProviders(<RouterProvider router={router} />);
  return { backend, router, user: userEvent.setup() };
}

const heading = (name: string) => screen.findByRole("heading", { level: 1, name });
const installButton = (title: string) => screen.getByRole("button", { name: `Install ${title}` });

async function openInstall(user: ReturnType<typeof userEvent.setup>, title: string) {
  await user.click(await screen.findByRole("button", { name: `Install ${title}` }));
  return screen.findByRole("dialog", { name: `Install ${title}` });
}

beforeEach(() => {
  localStorage.clear();
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(1000);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(800);
});

describe("package details", () => {
  it("shows the facts, a safe description and the native compatibility", async () => {
    setup(HARBOR);
    expect(await heading("Hollow Harbor")).toBeVisible();
    expect(screen.getByText("A cosy harbour town.")).toBeVisible();

    const facts = screen.getByRole("complementary", { name: "Details" });
    const fact = (label: string) => within(facts).getByText(label).nextElementSibling;
    expect(fact("Version")).toHaveTextContent("1.4.2");
    expect(fact("Size")).toHaveTextContent("12.0 GB");
    expect(fact("Platforms")).toHaveTextContent("Linux, Windows, Mac with Apple silicon");
    expect(fact("Developer")).toHaveTextContent("Lantern Works");
    expect(fact("Publisher")).toHaveTextContent("Unknown");

    // Server text is rendered as text: the HTML shows up literally and creates no element.
    const about = screen.getByRole("region", { name: "About" });
    expect((await within(about).findByText("Hollow Harbor")).tagName).toBe("STRONG");
    expect(about).toHaveTextContent('<img src=x onerror="alert(1)">');
    expect(about.querySelector("img")).toBeNull();

    expect(screen.getByRole("region", { name: "Compatibility" })).toHaveTextContent(
      "There's a version made for this computer.",
    );
  });

  it("shows Proton, the compatibility status with its notes, and the ProtonDB hint", async () => {
    setup(CANYON);
    await heading("Crimson Canyon");
    const compat = screen.getByRole("region", { name: "Compatibility" });
    expect(within(compat).getByText("Runs with Proton")).toBeVisible();
    expect(within(compat).getByText("Playable")).toBeVisible();
    expect(compat).toHaveTextContent("It runs, with some issues.");
    expect(within(compat).getByText(/Cutscenes stutter/)).toHaveTextContent(
      "Cutscenes stutter on first play. <b>not bold</b>",
    );
    expect(compat.querySelector("b")).toBeNull();
    expect(compat).toHaveTextContent("ProtonDB community rating: Gold");
    expect(compat).toHaveTextContent("Ratings come from other players, not from this server.");
    // The hero badge says how it runs too.
    expect(screen.getAllByText("Runs with Proton")).toHaveLength(2);
  });

  describe("no release for this platform", () => {
    it("says the package is not available and offers no install", async () => {
      setup(STATION);
      await heading("Silent Station");
      expect(screen.getAllByText("Not available on this computer")[0]).toBeVisible();
      const button = screen.getByRole("button", { name: "Not available on this computer" });
      expect(button).toHaveAttribute("aria-disabled", "true");
      expect(
        screen.getByText(/There's no version of this package for this computer/),
      ).toBeVisible();
      expect(screen.queryByRole("region", { name: "Compatibility" })).not.toBeInTheDocument();
    });

    it("explains it when the release went away after the page loaded", async () => {
      const { user, backend } = setup(HARBOR);
      await heading("Hollow Harbor");
      backend.state.packages = backend.state.packages.map((p) =>
        p.id === HARBOR.id ? { ...p, platforms: ["macos-aarch64"] } : p,
      );
      const dialog = await openInstall(user, "Hollow Harbor");
      expect(
        await within(dialog).findByText("There's no version of Hollow Harbor for this computer."),
      ).toBeVisible();
      expect(within(dialog).queryByRole("button", { name: "Install" })).not.toBeInTheDocument();
      expect(within(dialog).getByRole("button", { name: "Cancel" })).toBeVisible();
      expect(backend.callsTo("install_start")).toHaveLength(0);
    });
  });

  describe("install", () => {
    it("shows what is needed, picks the default library and queues the install", async () => {
      const { user, backend } = setup(HARBOR, { libraries: [MOCK_LIBRARY, OFFLINE_LIBRARY, SSD] });
      const dialog = await openInstall(user, "Hollow Harbor");
      expect(await within(dialog).findByText("Version 1.4.2", { exact: false })).toBeVisible();
      expect(dialog).toHaveTextContent("Linux version");
      expect(within(dialog).getByText("Download").nextElementSibling).toHaveTextContent("12.0 GB");

      const group = within(dialog).getByRole("radiogroup", { name: "Install to" });
      const home = within(group).getByRole("radio", { name: /\/home\/sam\/Games/ });
      expect(home).toBeChecked();
      // Focus moves from the footer to the choice once the plan has loaded.
      expect(home).toHaveFocus();
      expect(home).toHaveTextContent("412 GB free");
      expect(within(group).getByRole("radio", { name: /Fast SSD/ })).toHaveTextContent("/mnt/ssd");

      await user.click(within(group).getByRole("radio", { name: /Fast SSD/ }));
      await user.click(within(dialog).getByRole("button", { name: "Install" }));
      expect(await screen.findByText("Hollow Harbor is queued")).toBeVisible();
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
      expect(backend.callsTo("install_start")[0]?.args).toEqual({
        packageId: HARBOR.id,
        libraryId: SSD.id,
      });
      // The page now shows the install instead of the Install button.
      expect(await screen.findByRole("button", { name: "View download" })).toBeVisible();
    });

    it("lets the toast open the downloads", async () => {
      const { user, router } = setup(HARBOR);
      const dialog = await openInstall(user, "Hollow Harbor");
      await within(dialog).findByRole("radiogroup", { name: "Install to" });
      await user.click(within(dialog).getByRole("button", { name: "Install" }));
      await user.click(await screen.findByRole("button", { name: "View downloads" }));
      await waitFor(() => expect(router.state.location.pathname).toBe("/downloads"));
    });

    describe("insufficient space", () => {
      it("links to Storage settings when no library has room", async () => {
        const { user, router } = setup(ENGINE);
        const dialog = await openInstall(user, "Electric Engine");
        expect(
          await within(dialog).findByText("None of your libraries has enough space"),
        ).toBeVisible();
        const home = within(dialog).getByRole("radio", { name: /\/home\/sam\/Games/ });
        expect(home).toHaveAttribute("aria-disabled", "true");
        expect(home).toHaveTextContent("Not enough space");
        expect(within(dialog).queryByRole("button", { name: "Install" })).not.toBeInTheDocument();
        await user.click(within(dialog).getByRole("button", { name: "Open Storage settings" }));
        await waitFor(() => expect(router.state.location.pathname).toBe("/settings/storage"));
        expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
      });

      it("explains it when the drive filled up after the dialog opened", async () => {
        const { user, backend } = setup(HARBOR);
        const dialog = await openInstall(user, "Hollow Harbor");
        await within(dialog).findByRole("radiogroup", { name: "Install to" });
        backend.state.libraries = backend.state.libraries.map((l) =>
          l.id === MOCK_LIBRARY.id ? { ...l, free_bytes: 2 * GiB } : l,
        );
        await user.click(within(dialog).getByRole("button", { name: "Install" }));
        expect(await within(dialog).findByRole("alert")).toHaveTextContent(
          "Not enough space on that drive: 12.1 GB needed, 2.0 GB free.",
        );
        expect(screen.getByRole("dialog", { name: "Install Hollow Harbor" })).toBeVisible();
      });
    });

    describe("offline library", () => {
      it("cannot be chosen", async () => {
        const { user } = setup(HARBOR);
        const dialog = await openInstall(user, "Hollow Harbor");
        const external = await within(dialog).findByRole("radio", { name: /External drive/ });
        expect(external).toHaveAttribute("aria-disabled", "true");
        expect(external).toHaveTextContent("Drive not connected");
        await user.click(external);
        expect(external).not.toBeChecked();
        expect(within(dialog).getByRole("radio", { name: /\/home\/sam\/Games/ })).toBeChecked();
      });

      it("is reported when the drive was unplugged after the dialog opened", async () => {
        const { user, backend } = setup(HARBOR);
        const dialog = await openInstall(user, "Hollow Harbor");
        await within(dialog).findByRole("radiogroup", { name: "Install to" });
        backend.state.libraries = backend.state.libraries.map((l) =>
          l.id === MOCK_LIBRARY.id ? { ...l, online: false } : l,
        );
        await user.click(within(dialog).getByRole("button", { name: "Install" }));
        expect(await within(dialog).findByRole("alert")).toHaveTextContent(
          "The drive /home/sam/Games isn't connected.",
        );
      });

      it("leaves nothing to pick when every drive is offline", async () => {
        const { user } = setup(HARBOR, { libraries: [OFFLINE_LIBRARY] });
        const dialog = await openInstall(user, "Hollow Harbor");
        expect(
          await within(dialog).findByText("None of your libraries has enough space"),
        ).toBeVisible();
      });
    });

    describe("package removed while viewing", () => {
      it("switches the page to the removed state when the dialog finds out", async () => {
        const { user, backend, router } = setup(HARBOR);
        await heading("Hollow Harbor");
        backend.state.removedPackages = [HARBOR.id];
        const dialog = await openInstall(user, "Hollow Harbor");
        expect(
          await within(dialog).findByText("This package was removed from the server."),
        ).toBeVisible();
        expect(within(dialog).queryByRole("button", { name: "Install" })).not.toBeInTheDocument();
        await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
        expect(
          await screen.findByRole("heading", { name: "This package isn't available anymore" }),
        ).toBeVisible();
        // The Install button is gone: focus goes to the page title, not nowhere.
        expect(screen.getByRole("heading", { level: 1, name: "Hollow Harbor" })).toHaveFocus();
        await user.click(screen.getByRole("button", { name: "Browse the catalog" }));
        await waitFor(() => expect(router.state.location.pathname).toBe("/browse"));
      });

      it("reports it when the install is confirmed", async () => {
        const { user, backend } = setup(HARBOR);
        const dialog = await openInstall(user, "Hollow Harbor");
        await within(dialog).findByRole("radiogroup", { name: "Install to" });
        backend.state.removedPackages = [HARBOR.id];
        await user.click(within(dialog).getByRole("button", { name: "Install" }));
        expect(await within(dialog).findByRole("alert")).toHaveTextContent(
          "This package was removed from the server.",
        );
        expect(within(dialog).queryByRole("button", { name: "Install" })).not.toBeInTheDocument();
        expect(backend.state.installs).toHaveLength(0);
        await user.keyboard("{Escape}");
        expect(
          await screen.findByRole("heading", { name: "This package isn't available anymore" }),
        ).toBeVisible();
      });

      it("shows the removed state when the page opens on a removed package", async () => {
        setup(HARBOR, { removedPackages: [HARBOR.id] });
        expect(
          await screen.findByRole("heading", { name: "This package isn't available anymore" }),
        ).toBeVisible();
        expect(screen.queryByRole("button", { name: /^Install/ })).not.toBeInTheDocument();
      });
    });

    it("offers another try when the server could not be reached", async () => {
      let offline = true;
      const { user, backend } = setup(HARBOR);
      const real = catalogHandlers(backend.state).install_plan;
      backend.on("install_plan", (args) =>
        offline ? Promise.reject({ kind: "offline" }) : real?.(args),
      );
      const dialog = await openInstall(user, "Hollow Harbor");
      expect(await within(dialog).findByRole("alert")).toHaveTextContent(
        "The server can't be reached. Try again when you're back online.",
      );
      offline = false;
      await user.click(within(dialog).getByRole("button", { name: "Try again" }));
      expect(await within(dialog).findByRole("radiogroup", { name: "Install to" })).toBeVisible();
      expect(within(dialog).getByRole("button", { name: "Install" })).not.toHaveAttribute(
        "aria-disabled",
      );
    });

    it("explains that new installs are paused when the server's trust expired", async () => {
      const { user } = setup(HARBOR, { trustExpired: true });
      const dialog = await openInstall(user, "Hollow Harbor");
      await within(dialog).findByRole("radiogroup", { name: "Install to" });
      await user.click(within(dialog).getByRole("button", { name: "Install" }));
      expect(await within(dialog).findByRole("alert")).toHaveTextContent(
        /new installs are paused\. Games you already installed still work\./,
      );
    });
  });

  describe("already installed", () => {
    const withUpdate = () =>
      setup(HARBOR, {
        installs: [
          installed(HARBOR, {
            version_label: "1.3.0",
            update: {
              version_label: "1.4.2",
              sequence: 9,
              download_bytes: GiB,
              installed_yanked: false,
            },
          }),
        ],
      });

    it("offers Play instead of Install", async () => {
      const { user, backend } = withUpdate();
      await heading("Hollow Harbor");
      expect(
        screen.queryByRole("button", { name: "Install Hollow Harbor" }),
      ).not.toBeInTheDocument();
      expect(screen.getByText("Installed · version 1.3.0")).toBeVisible();
      await user.click(screen.getByRole("button", { name: "Play Hollow Harbor" }));
      await waitFor(() => expect(backend.callsTo("game_launch")).toHaveLength(1));
    });

    it("offers the update and the library actions, without a link to this page", async () => {
      const { user, backend } = withUpdate();
      await heading("Hollow Harbor");
      await user.click(screen.getByRole("button", { name: "More actions for Hollow Harbor" }));
      const menu = screen.getByRole("menu", { name: "Actions for Hollow Harbor" });
      expect(within(menu).queryByRole("menuitem", { name: "Details" })).not.toBeInTheDocument();
      expect(within(menu).getByRole("menuitem", { name: "Uninstall…" })).toBeVisible();
      await user.click(within(menu).getByRole("menuitem", { name: "Update to 1.4.2" }));
      await waitFor(() => expect(backend.callsTo("install_update")).toHaveLength(1));
      expect(await screen.findByText("The update for Hollow Harbor is queued")).toBeVisible();
    });
  });

  describe("blockers", () => {
    it("blocks DirectX 12 games on an Intel Mac", async () => {
      const { user } = setup(GARDEN, { host: "macos-x86_64" });
      await heading("Gilded Garden");
      const compat = screen.getByRole("region", { name: "Compatibility" });
      expect(compat).toHaveTextContent("Runs with Wine");
      expect(compat).toHaveTextContent(
        "DirectX 12 games need a Mac with Apple silicon. This game uses DirectX 12",
      );
      const install = installButton("Gilded Garden");
      expect(install).toHaveAttribute("aria-disabled", "true");
      // Why, right under the button (and in the tooltip).
      expect(install.closest("div")?.nextElementSibling).toHaveTextContent(
        "DirectX 12 games need a Mac with Apple silicon.",
      );
      await user.click(install);
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
      await user.hover(install);
      expect(await screen.findByRole("tooltip")).toHaveTextContent(
        "DirectX 12 games need a Mac with Apple silicon.",
      );
    });

    it("installs Rosetta 2 when it is missing, then allows the install", async () => {
      const { user, backend } = setup(FORGE, { host: "macos-aarch64", rosettaInstalled: false });
      await heading("Frozen Forge");
      expect(screen.getByText("Runs with Rosetta 2")).toBeVisible();
      const compat = screen.getByRole("region", { name: "Compatibility" });
      expect(compat).toHaveTextContent("Needs Rosetta 2.");
      expect(compat).toHaveTextContent("May stop working after macOS 27.");
      expect(installButton("Frozen Forge")).toHaveAttribute("aria-disabled", "true");

      await user.click(within(compat).getByRole("button", { name: "Install Rosetta 2" }));
      const confirm = screen.getByRole("alertdialog", { name: "Install Rosetta 2?" });
      await user.click(within(confirm).getByRole("button", { name: "Install Rosetta 2" }));
      expect(await screen.findByText("Rosetta 2 is installed")).toBeVisible();
      expect(backend.callsTo("rosetta_install")).toHaveLength(1);
      await waitFor(() =>
        expect(screen.getByRole("button", { name: "Install Frozen Forge" })).not.toHaveAttribute(
          "aria-disabled",
        ),
      );
      // The sunset warning is information only.
      expect(compat).toHaveTextContent("May stop working after macOS 27.");
      expect(compat).not.toHaveTextContent("Needs Rosetta 2.");
    });

    it("says why Rosetta 2 could not be installed", async () => {
      const { user, backend } = setup(FORGE, { host: "macos-aarch64", rosettaInstalled: false });
      backend.on("rosetta_install", () =>
        Promise.reject({ kind: "io", detail: "softwareupdate exited with 1" }),
      );
      await heading("Frozen Forge");
      await user.click(screen.getByRole("button", { name: "Install Rosetta 2" }));
      await user.click(
        within(screen.getByRole("alertdialog")).getByRole("button", { name: "Install Rosetta 2" }),
      );
      expect(
        await screen.findByText("Rosetta 2 couldn't be installed: softwareupdate exited with 1"),
      ).toBeVisible();
    });
  });

  describe("screenshots", () => {
    it("steps through the viewer with the keyboard and returns focus on close", async () => {
      const { user } = setup(HARBOR);
      await heading("Hollow Harbor");
      const total = HARBOR.screenshots.length;
      const shots = screen.getByRole("region", { name: "Screenshots" });
      const second = within(shots).getByRole("button", { name: `Screenshot 2 of ${total}` });
      await user.click(second);
      const viewer = screen.getByRole("dialog", { name: "Screenshots of Hollow Harbor" });
      expect(viewer).toHaveAccessibleDescription(`2 of ${total}`);
      expect(within(viewer).getByRole("button", { name: "Next screenshot" })).toHaveFocus();
      expect(within(viewer).getByRole("img")).toHaveAccessibleName(`Screenshot 2 of ${total}`);

      await user.keyboard("{ArrowRight}");
      expect(viewer).toHaveAccessibleDescription(`3 of ${total}`);
      await user.keyboard("{ArrowLeft}{ArrowLeft}{ArrowLeft}");
      expect(viewer).toHaveAccessibleDescription(`${total} of ${total}`);
      await user.keyboard("{Control>}{PageDown}{/Control}");
      expect(viewer).toHaveAccessibleDescription(`1 of ${total}`);
      await user.click(within(viewer).getByRole("button", { name: "Next screenshot" }));
      expect(viewer).toHaveAccessibleDescription(`2 of ${total}`);

      await user.keyboard("{Escape}");
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
      expect(second).toHaveFocus();
    });

    it("has no screenshots section when there are none", async () => {
      setup(ENGINE);
      await heading("Electric Engine");
      expect(screen.queryByRole("region", { name: "Screenshots" })).not.toBeInTheDocument();
    });
  });

  it("goes back to where the player came from", async () => {
    const { user, router } = setup(HARBOR);
    await heading("Hollow Harbor");
    await user.click(screen.getByRole("button", { name: "Back" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/browse"));
  });

  it("shows an error with a retry when the details cannot be loaded", async () => {
    let failing = true;
    const backend = installMockBackend({
      servers: [MOCK_SERVER],
      libraries: [MOCK_LIBRARY],
      packages: [HARBOR],
    });
    backend.on("package_details", () =>
      failing
        ? Promise.reject({ kind: "offline" })
        : Promise.resolve({
            package_id: HARBOR.id,
            slug: HARBOR.slug,
            title: HARBOR.title,
            summary: null,
            description: null,
            developer: null,
            publisher: null,
            release_date: null,
            genres: [],
            platforms: HARBOR.platforms,
            cover_url: null,
            hero_url: null,
            logo_url: null,
            screenshots: [],
            release: null,
            compat: { kind: "native" },
          }),
    );
    const router = createMemoryRouter(routes, { initialEntries: [`/package/${HARBOR.id}`] });
    renderWithProviders(<RouterProvider router={router} />);
    const user = userEvent.setup();
    expect(
      await screen.findByRole("heading", { name: "Couldn't load this package" }),
    ).toBeVisible();
    failing = false;
    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(await heading("Hollow Harbor")).toBeVisible();
    expect(
      screen.getByText("The server's admins haven't written a description yet."),
    ).toBeVisible();
  });
});
