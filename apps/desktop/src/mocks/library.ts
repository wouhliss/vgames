// Library fixtures: installed packages, collections, launching and install actions.
import type {
  Collection,
  CollectionError,
  InstallActionError,
  InstalledPackage,
  LaunchError,
  LibraryInfo,
  PackageRef,
  ServerProfile,
  UninstallPlan,
} from "../ipc";
import { events } from "../ipc";
import { fail, type Handler, later, mockId, seeded } from "./runtime";

export interface LibraryState {
  installs: InstalledPackage[];
  collections: Collection[];
  /** Forced launch results by package id (otherwise the launch succeeds). */
  launchErrors: Record<string, LaunchError>;
  /** Forced results for install actions by `<command>:<package id>`. */
  actionErrors: Record<string, InstallActionError>;
  /** How long simulated background work takes (ms). 0 = immediately (unit tests). */
  actionDelayMs: number;
}

export function defaultLibraryState(): LibraryState {
  return { installs: [], collections: [], launchErrors: {}, actionErrors: {}, actionDelayMs: 800 };
}

const ADJECTIVES = [
  "Hollow",
  "Crimson",
  "Silent",
  "Gilded",
  "Frozen",
  "Wandering",
  "Broken",
  "Electric",
  "Sunken",
  "Radiant",
  "Last",
  "Midnight",
  "Iron",
  "Paper",
  "Velvet",
  "Distant",
  "Clockwork",
  "Emerald",
  "Feral",
  "Quiet",
  "Neon",
  "Ashen",
  "Tidal",
  "Hidden",
  "Glass",
  "Starlit",
  "Rusty",
  "Wild",
  "Lucky",
  "Endless",
  "Tiny",
  "Grand",
  "Lost",
  "Northern",
  "Scarlet",
  "Hungry",
  "Burning",
  "Secret",
  "Copper",
  "Pale",
  "Stormy",
  "Golden",
  "Salt",
  "Violet",
  "Ancient",
  "Brave",
  "Cosmic",
  "Drowsy",
  "Humble",
  "Sly",
];
const NOUNS = [
  "Cartographer",
  "Lighthouse",
  "Orchard",
  "Engine",
  "Kingdom",
  "Harbor",
  "Circuit",
  "Labyrinth",
  "Caravan",
  "Observatory",
  "Foundry",
  "Garden",
  "Frontier",
  "Monastery",
  "Station",
  "Archive",
  "Voyage",
  "Citadel",
  "Bazaar",
  "Tundra",
  "Reef",
  "Colony",
  "Parade",
  "Signal",
  "Workshop",
  "Carnival",
  "Outpost",
  "Canyon",
  "Nebula",
  "Village",
  "Expedition",
  "Tower",
  "Railway",
  "Swamp",
  "Galleon",
  "Arena",
  "Dungeon",
  "Meadow",
  "Reactor",
  "Glacier",
  "Mine",
  "Sanctuary",
  "Market",
  "Skyline",
  "Grove",
  "Fortress",
  "Delta",
  "Atelier",
  "Beacon",
  "Island",
  "Bakery",
  "Circus",
  "Harvest",
  "Kitchen",
  "Planet",
  "Spire",
  "Theatre",
  "Valley",
  "Warden",
  "Zephyr",
  "Quarry",
  "Relay",
  "Ferry",
  "Hangar",
  "Lagoon",
  "Oasis",
  "Pagoda",
  "Rampart",
  "Summit",
  "Thicket",
  "Vault",
  "Wharf",
  "Aviary",
  "Bastion",
  "Cavern",
  "Dockyard",
  "Estate",
  "Fjord",
  "Gorge",
  "Hamlet",
  "Inn",
  "Jungle",
  "Keep",
  "Lodge",
  "Manor",
  "Nest",
  "Pier",
  "Ridge",
  "Shrine",
  "Temple",
  "Utopia",
  "Vista",
  "Wilds",
  "Yard",
  "Abyss",
  "Bridge",
  "Cabin",
  "Depot",
  "Eclipse",
  "Forge",
];

/** Titles that exercise the UI: unicode, right-to-left and very long names. */
const SPECIAL_TITLES = [
  "Ōkami no Michi: Director's Cut",
  "Überfall auf Schloss Größenwahn",
  "مغامرة الصحراء",
  "The Extraordinarily Long-Winded Chronicles of the Seven Sleepy Cartographers and Their Very Patient Llama",
  "🚀 Rocket Rally",
];

export const MOCK_COLLECTIONS: Collection[] = [
  { id: mockId("01920000-0000-7000-c011", 1), name: "Co-op nights", position: 0 },
  { id: mockId("01920000-0000-7000-c011", 2), name: "Backlog", position: 1 },
  { id: mockId("01920000-0000-7000-c011", 3), name: "Finished", position: 2 },
];

export const OFFLINE_LIBRARY: LibraryInfo = {
  id: "01920000-0000-7000-8000-0000000000b2",
  path: "/media/sam/External",
  label: "External drive",
  is_default: false,
  online: false,
  free_bytes: null,
  total_bytes: null,
  install_count: 0,
};

/** The title of fixture package `i`; the first few are unicode, right-to-left and long names. */
export function titleFor(i: number, withSpecials: boolean): string {
  const special = withSpecials && i < SPECIAL_TITLES.length ? SPECIAL_TITLES[i] : undefined;
  if (special) return special;
  // (i mod 50, (51·i + ⌊i/50⌋) mod 100) is a distinct pair for every i < 5,000.
  const base = `${ADJECTIVES[i % ADJECTIVES.length]} ${NOUNS[(i * 51 + Math.floor(i / ADJECTIVES.length)) % NOUNS.length]}`;
  const round = Math.floor(i / (ADJECTIVES.length * NOUNS.length));
  return round > 0 ? `${base} ${round + 1}` : base;
}

export function slugFor(title: string, i: number): string {
  const slug = title
    .toLowerCase()
    .normalize("NFKD")
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
  return slug || `package-${i + 1}`;
}

/** Package ids are shared by the library and catalog fixtures: installed package i is catalog entry i. */
export function packageIdFor(i: number): string {
  return mockId("0192a6f0-1c2d-7e3f-8a9b", i + 1);
}

export const DAY = 86_400_000;
export const BASE_TIME = Date.parse("2026-09-24T12:00:00Z");

/** `count` installed packages on `server` spread over `libraries`, deterministic. */
export function makeInstalls(
  count: number,
  server: ServerProfile,
  libraries: readonly LibraryInfo[],
): InstalledPackage[] {
  const random = seeded(42);
  const collectionIds = MOCK_COLLECTIONS.map((c) => c.id);
  const out: InstalledPackage[] = [];
  for (let i = 0; i < count; i += 1) {
    const title = titleFor(i, count > 20);
    const library = libraries[i % 13 === 5 ? libraries.length - 1 : 0] ?? libraries[0];
    const played = random() > 0.3;
    const installedAt = BASE_TIME - Math.floor(random() * 400) * DAY;
    const state: InstalledPackage["state"] = i % 23 === 7 ? "incomplete" : "installed";
    const slug = slugFor(title, i);
    out.push({
      package: { server_id: server.id, package_id: packageIdFor(i) },
      slug,
      title,
      cover_url: null,
      library_id: library?.id ?? "",
      install_path: `${library?.path ?? "/games"}/${slug}`,
      platform: i % 9 === 4 ? "windows-x86_64" : "linux-x86_64",
      version_label: `1.${i % 7}.${Math.floor(random() * 10)}`,
      sequence: 3 + (i % 5),
      size_bytes: Math.floor((0.2 + random() * 60) * 1024 ** 3),
      installed_at: state === "incomplete" ? null : new Date(installedAt).toISOString(),
      last_played_at:
        played && state === "installed"
          ? new Date(installedAt + Math.floor(random() * 30) * DAY).toISOString()
          : null,
      playtime_seconds: played ? Math.floor(random() * 200 * 3600) : 0,
      state,
      update:
        i % 11 === 3 && state === "installed"
          ? {
              version_label: "2.0.0",
              sequence: 9,
              download_bytes: Math.floor(random() * 4 * 1024 ** 3),
              installed_yanked: i === 14,
            }
          : null,
      favorite: i % 17 === 2,
      collection_ids: collectionIds.filter((_, c) => (i + c) % (5 + c * 3) === 0),
      running: false,
      targets:
        state === "incomplete"
          ? []
          : i % 6 === 1
            ? [
                { id: "play", label: "Play", is_default: true },
                { id: "editor", label: "Level editor", is_default: false },
                { id: "safe", label: "Safe mode", is_default: false },
              ]
            : [{ id: "play", label: "Play", is_default: true }],
      compat: i % 9 === 4 ? "proton" : "native",
      cloud_saves:
        i % 19 === 6
          ? "pending"
          : i % 29 === 8
            ? "conflict"
            : i % 3 === 0
              ? "synced"
              : "unsupported",
    });
  }
  return out;
}

export function makeUninstallPlan(pkg: InstalledPackage): UninstallPlan {
  const withLeftovers = pkg.title.length % 2 === 0;
  const leftovers = withLeftovers
    ? [
        { path: "Saved/profile.sav", size_bytes: 48_213 },
        { path: "Mods/better-maps/readme.txt", size_bytes: 1_020 },
        { path: "settings.ini", size_bytes: 612 },
      ]
    : [];
  return {
    size_bytes: pkg.size_bytes,
    leftovers,
    leftover_count: leftovers.length,
    leftover_bytes: leftovers.reduce((sum, f) => sum + f.size_bytes, 0),
    has_prefix: pkg.compat !== "native",
  };
}

function sameRef(a: PackageRef, b: unknown): boolean {
  const r = b as PackageRef | null;
  return r !== null && a.server_id === r.server_id && a.package_id === r.package_id;
}

function validName(raw: unknown): string | null {
  const name = String(raw ?? "").trim();
  const length = [...name].length;
  return length >= 1 && length <= 100 ? name : null;
}

export function libraryHandlers(
  state: LibraryState & { libraries: LibraryInfo[] },
): Record<string, Handler> {
  const changed = () => void events.installsChanged.emit({});
  const collectionsChanged = () => void events.collectionsChanged.emit({});

  const find = (ref: unknown): InstalledPackage => {
    const pkg = state.installs.find((i) => sameRef(i.package, ref));
    if (!pkg) fail({ kind: "not_found" } satisfies InstallActionError);
    return pkg;
  };
  const update = (ref: PackageRef, patch: Partial<InstalledPackage>) => {
    state.installs = state.installs.map((i) => (sameRef(i.package, ref) ? { ...i, ...patch } : i));
    changed();
  };
  const forced = (cmd: string, ref: unknown) => {
    const error = state.actionErrors[`${cmd}:${(ref as PackageRef).package_id}`];
    if (error) fail(error);
  };
  const libraryOf = (pkg: InstalledPackage) => state.libraries.find((l) => l.id === pkg.library_id);
  const requireIdle = (pkg: InstalledPackage) => {
    if (pkg.running) fail({ kind: "running" } satisfies InstallActionError);
    if (pkg.state !== "installed" && pkg.state !== "incomplete")
      fail({ kind: "busy", state: pkg.state } satisfies InstallActionError);
    const library = libraryOf(pkg);
    if (library && !library.online)
      fail({ kind: "library_offline", library_path: library.path } satisfies InstallActionError);
  };
  const collectionOrFail = (id: unknown) => {
    const collection = state.collections.find((c) => c.id === id);
    if (!collection) fail({ kind: "not_found" } satisfies CollectionError);
    return collection;
  };
  let pid = 4000;

  return {
    installs_list: () => state.installs,
    collections_list: () => [...state.collections].sort((a, b) => a.position - b.position),
    collection_create: (args) => {
      const name = validName(args.name);
      if (!name) fail({ kind: "invalid_name" } satisfies CollectionError);
      if (state.collections.some((c) => c.name.toLowerCase() === name.toLowerCase()))
        fail({ kind: "name_taken" } satisfies CollectionError);
      const collection: Collection = {
        id: mockId("01920000-0000-7000-c011", 100 + state.collections.length + (Date.now() % 1000)),
        name,
        position: state.collections.length,
      };
      state.collections = [...state.collections, collection];
      collectionsChanged();
      return collection;
    },
    collection_rename: (args) => {
      const collection = collectionOrFail(args.collectionId);
      const name = validName(args.name);
      if (!name) fail({ kind: "invalid_name" } satisfies CollectionError);
      if (
        state.collections.some(
          (c) => c.id !== collection.id && c.name.toLowerCase() === name.toLowerCase(),
        )
      )
        fail({ kind: "name_taken" } satisfies CollectionError);
      const renamed = { ...collection, name };
      state.collections = state.collections.map((c) => (c.id === collection.id ? renamed : c));
      collectionsChanged();
      return renamed;
    },
    collection_delete: (args) => {
      const collection = collectionOrFail(args.collectionId);
      state.collections = state.collections
        .filter((c) => c.id !== collection.id)
        .map((c, position) => ({ ...c, position }));
      state.installs = state.installs.map((i) => ({
        ...i,
        collection_ids: i.collection_ids.filter((id) => id !== collection.id),
      }));
      collectionsChanged();
      changed();
      return null;
    },
    collections_reorder: (args) => {
      const ids = args.collectionIds as string[];
      const ordered = [
        ...ids
          .map((id) => state.collections.find((c) => c.id === id))
          .filter((c) => c !== undefined),
        ...state.collections.filter((c) => !ids.includes(c.id)),
      ];
      state.collections = ordered.map((c, position) => ({ ...c, position }));
      collectionsChanged();
      return null;
    },
    collection_add_package: (args) => {
      const collection = collectionOrFail(args.collectionId);
      const pkg = find(args.package);
      if (!pkg.collection_ids.includes(collection.id))
        update(pkg.package, { collection_ids: [...pkg.collection_ids, collection.id] });
      return null;
    },
    collection_remove_package: (args) => {
      const collection = collectionOrFail(args.collectionId);
      const pkg = find(args.package);
      update(pkg.package, {
        collection_ids: pkg.collection_ids.filter((id) => id !== collection.id),
      });
      return null;
    },
    favorite_set: (args) => {
      const pkg = find(args.package);
      update(pkg.package, { favorite: Boolean(args.favorite) });
      return null;
    },

    game_launch: (args) => {
      const pkg = state.installs.find((i) => sameRef(i.package, args.pkg));
      if (!pkg) fail({ kind: "not_installed" } satisfies LaunchError);
      const forcedError = state.launchErrors[pkg.package.package_id];
      if (forcedError) fail(forcedError);
      if (pkg.running) fail({ kind: "already_running" } satisfies LaunchError);
      if (pkg.state === "incomplete") fail({ kind: "incomplete" } satisfies LaunchError);
      if (pkg.state !== "installed") fail({ kind: "busy", state: pkg.state } satisfies LaunchError);
      const library = libraryOf(pkg);
      if (library && !library.online)
        fail({ kind: "library_offline", library_path: library.path } satisfies LaunchError);
      const target = args.targetId as string | null;
      if (target !== null && !pkg.targets.some((t) => t.id === target))
        fail({ kind: "target_not_found" } satisfies LaunchError);
      update(pkg.package, { running: true, last_played_at: new Date().toISOString() });
      pid += 1;
      void events.gameStarted.emit({ package: pkg.package, pid });
      return null;
    },
    game_stop: (args) => {
      const pkg = find(args.pkg);
      update(pkg.package, { running: false });
      void events.gameStopped.emit({
        package: pkg.package,
        exit: { code: null, stopped_by_user: true, session_seconds: 60 },
      });
      return null;
    },

    install_update: (args) => {
      forced("install_update", args.package);
      const pkg = find(args.package);
      requireIdle(pkg);
      update(pkg.package, { state: "updating" });
      later(state.actionDelayMs * 3, () => {
        const current = state.installs.find((i) => sameRef(i.package, pkg.package));
        if (!current?.update) return;
        update(pkg.package, {
          state: "installed",
          version_label: current.update.version_label,
          sequence: current.update.sequence,
          update: null,
        });
      });
      return null;
    },
    install_verify: (args) => {
      forced("install_verify", args.package);
      const pkg = find(args.package);
      requireIdle(pkg);
      update(pkg.package, { state: "repairing" });
      later(state.actionDelayMs, () => update(pkg.package, { state: "installed" }));
      return null;
    },
    install_resume: (args) => {
      forced("install_resume", args.package);
      const pkg = find(args.package);
      requireIdle(pkg);
      update(pkg.package, { state: "installing" });
      return null;
    },
    install_move: (args) => {
      forced("install_move", args.package);
      const pkg = find(args.package);
      requireIdle(pkg);
      if (args.libraryId === pkg.library_id)
        fail({ kind: "same_library" } satisfies InstallActionError);
      const target = state.libraries.find((l) => l.id === args.libraryId);
      if (!target) fail({ kind: "not_found" } satisfies InstallActionError);
      if (!target.online)
        fail({ kind: "library_offline", library_path: target.path } satisfies InstallActionError);
      if (target.free_bytes !== null && target.free_bytes < pkg.size_bytes)
        fail({
          kind: "insufficient_space",
          required_bytes: pkg.size_bytes,
          available_bytes: target.free_bytes,
        } satisfies InstallActionError);
      update(pkg.package, { state: "moving" });
      later(state.actionDelayMs, () =>
        update(pkg.package, {
          state: "installed",
          library_id: target.id,
          install_path: `${target.path}/${pkg.slug}`,
        }),
      );
      return null;
    },
    install_uninstall_plan: (args) => {
      forced("install_uninstall_plan", args.package);
      const pkg = find(args.package);
      requireIdle(pkg);
      return makeUninstallPlan(pkg);
    },
    install_uninstall: (args) => {
      forced("install_uninstall", args.package);
      const pkg = find(args.package);
      requireIdle(pkg);
      update(pkg.package, { state: "uninstalling" });
      later(state.actionDelayMs, () => {
        state.installs = state.installs.filter((i) => !sameRef(i.package, pkg.package));
        changed();
      });
      return null;
    },
    install_open_folder: (args) => {
      find(args.package);
      return null;
    },
    shortcut_create: (args) => {
      const pkg = find(args.pkg);
      return `/home/sam/Desktop/${pkg.title}.desktop`;
    },
  };
}
