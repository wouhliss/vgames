// Catalog fixtures: the server's published packages as this machine sees them, package details with
// compatibility per host, install plans and queued installs. Package i here is installed package i in
// the library fixtures, so the "ready" preset shows its 40 installs as installed in Browse.
import type {
  Availability,
  CatalogError,
  CatalogItem,
  CatalogQuery,
  CompatBlocker,
  CompatInfo,
  CompatStatus,
  GenreCount,
  HostRelease,
  InstalledPackage,
  InstallPlan,
  InstallPlanError,
  InstallStartError,
  LibraryInfo,
  PackageDetails,
  Platform,
  ProtonDbTier,
  Screenshot,
  ServerProfile,
} from "../ipc";
import { events } from "../ipc";
import { type DownloadsState, makeJob, queueInstall } from "./downloads";
import { BASE_TIME, DAY, packageIdFor, slugFor, titleFor } from "./library";
import { fail, type Handler, mockId, seeded } from "./runtime";

export const GENRES = [
  "Action",
  "Adventure",
  "Puzzle",
  "Strategy",
  "Racing",
  "Simulation",
  "Platformer",
  "Role-playing",
] as const;

const STUDIOS = [
  "Lantern Works",
  "Tiny Harbor Games",
  "Northwind Interactive",
  "Paper Crane Studio",
  "Quiet Ferret",
];

const NOTES = [
  "Controller works out of the box. Cutscenes play normally.",
  "Runs well. The launcher from the original release is skipped automatically.",
  null,
  "Crashes after the first level with every tested runtime.",
];

/** A published package as the mock server holds it. */
export interface MockPackage {
  id: string;
  slug: string;
  title: string;
  summary: string;
  description: string;
  developer: string | null;
  publisher: string | null;
  release_date: string | null;
  genres: string[];
  platforms: Platform[];
  updated_at: string;
  screenshots: Screenshot[];
  total_size: number;
  version_label: string;
  sequence: number;
  /** Uses Direct3D 12 (only D3DMetal runs it on a Mac, and only on Apple silicon). */
  d3d12: boolean;
  profile: { status: CompatStatus; notes: string | null } | null;
  protondb: ProtonDbTier | null;
}

export interface CatalogState {
  packages: MockPackage[];
  /** The machine the mock launcher runs on. */
  host: Platform;
  rosettaInstalled: boolean;
  /** Package ids that disappeared from the server (unpublished) after the UI loaded them. */
  removedPackages: string[];
  /** The server's trust bundle expired: new installs are refused. */
  trustExpired: boolean;
  /** Page size of `catalog_list`. */
  catalogPageSize: number;
}

function screenshot(hue: number, n: number): Screenshot {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="720" viewBox="0 0 1280 720"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="hsl(${hue} 55% 38%)"/><stop offset="1" stop-color="hsl(${(hue + 50) % 360} 60% 14%)"/></linearGradient></defs><rect width="1280" height="720" fill="url(#g)"/><circle cx="${260 + n * 120}" cy="${300 + (n % 3) * 60}" r="${90 + n * 12}" fill="hsl(${(hue + 180) % 360} 60% 60% / 0.35)"/><text x="64" y="660" font-family="sans-serif" font-size="48" fill="white" opacity="0.8">Screenshot ${n}</text></svg>`;
  return { url: `data:image/svg+xml,${encodeURIComponent(svg)}`, width: 1280, height: 720 };
}

const article = (word: string) => (/^[aeiou]/i.test(word) ? "an" : "a");
const capitalize = (word: string) => word.charAt(0).toUpperCase() + word.slice(1);

function description(title: string, genre: string, players: number, i: number): string {
  if (i === 6) {
    // Server text is untrusted: raw HTML must show as text, never run.
    return `**${title}** has a description with HTML in it.\n\n<img src=x onerror="alert(1)">\n\n<script>alert("no")</script>`;
  }
  const long =
    i === 4
      ? "\n\n## Patch notes\n\n" +
        Array.from(
          { length: 12 },
          (_, n) =>
            `- Version 1.${n}: fixed a rare crash when loading a saved game, and improved controller support.`,
        ).join("\n")
      : "";
  return `${title} is ${article(genre)} ${genre.toLowerCase()} game for ${players === 1 ? "one player" : `up to ${players} players`}.\n\n## Features\n\n- Split-screen and online play\n- Full controller support\n- Cloud saves\n\nMore on [the studio's site](https://example.org/games/${i}).${long}`;
}

export function makeCatalog(count: number): MockPackage[] {
  const random = seeded(7);
  const out: MockPackage[] = [];
  for (let i = 0; i < count; i += 1) {
    const title = titleFor(i, true);
    const genre = GENRES[i % GENRES.length] ?? "Action";
    const genres = i % 3 === 0 ? [genre, GENRES[(i * 3 + 1) % GENRES.length] ?? "Puzzle"] : [genre];
    // Must agree with the library fixtures for installed packages (i < 40): i % 9 === 4 is a
    // Windows-only build (runs with Proton on Linux); the rest have a native Linux build.
    const platforms: Platform[] =
      i % 9 === 4
        ? ["windows-x86_64"]
        : i >= 40 && i % 7 === 5
          ? ["macos-aarch64"]
          : i % 5 === 2
            ? ["linux-x86_64", "windows-x86_64", "macos-x86_64"]
            : ["linux-x86_64", "windows-x86_64", "macos-aarch64"];
    const players = 1 + (i % 4);
    const hue = Math.floor(random() * 360);
    out.push({
      id: packageIdFor(i),
      slug: slugFor(title, i),
      title,
      summary: `${capitalize(article(genre))} ${genre.toLowerCase()} game for ${players === 1 ? "one player" : `up to ${players} players`}.`,
      description: description(title, genre, players, i),
      developer: STUDIOS[i % STUDIOS.length] ?? null,
      publisher: i % 4 === 0 ? null : (STUDIOS[(i + 2) % STUDIOS.length] ?? null),
      release_date:
        i % 6 === 5 ? null : new Date(BASE_TIME - (i * 11 + 30) * DAY).toISOString().slice(0, 10),
      genres,
      platforms,
      updated_at: new Date(BASE_TIME - Math.floor(random() * 200) * DAY).toISOString(),
      screenshots: Array.from({ length: i % 5 === 1 ? 0 : 3 + (i % 4) }, (_, n) =>
        screenshot((hue + n * 25) % 360, n + 1),
      ),
      total_size: Math.floor((0.3 + random() * 55) * 1024 ** 3),
      version_label: `1.${i % 7}.${Math.floor(random() * 10)}`,
      sequence: 3 + (i % 5),
      d3d12: i % 10 === 3,
      profile:
        i % 4 === 2
          ? null
          : {
              status:
                (["verified", "playable", "untested", "unsupported"] as const)[i % 4] ?? "untested",
              notes: NOTES[i % 4] ?? null,
            },
      protondb: platforms.includes("windows-x86_64")
        ? ((["platinum", "gold", "silver", "bronze", "borked", "pending"] as const)[i % 6] ?? null)
        : null,
    });
  }
  return out;
}

export function defaultCatalogState(): CatalogState {
  return {
    packages: [],
    host: "linux-x86_64",
    rosettaInstalled: true,
    removedPackages: [],
    trustExpired: false,
    catalogPageSize: 24,
  };
}

/** Which build runs here and how (09-compatibility §1). */
export function releaseChoice(
  platforms: readonly Platform[],
  host: Platform,
): { platform: Platform; via: Availability } | null {
  const has = (p: Platform) => platforms.includes(p);
  const order: Record<Platform, [Platform, Availability][]> = {
    "windows-x86_64": [["windows-x86_64", "native"]],
    "windows-aarch64": [
      ["windows-aarch64", "native"],
      ["windows-x86_64", "native"],
    ],
    "linux-x86_64": [
      ["linux-x86_64", "native"],
      ["windows-x86_64", "proton"],
    ],
    "linux-aarch64": [["linux-aarch64", "native"]],
    "macos-aarch64": [
      ["macos-aarch64", "native"],
      ["macos-x86_64", "rosetta"],
      ["windows-x86_64", "wine"],
    ],
    "macos-x86_64": [
      ["macos-x86_64", "native"],
      ["windows-x86_64", "wine"],
    ],
  };
  const match = order[host].find(([p]) => has(p));
  return match ? { platform: match[0], via: match[1] } : null;
}

function compatFor(pkg: MockPackage, state: CatalogState): CompatInfo {
  const choice = releaseChoice(pkg.platforms, state.host);
  if (!choice) return { kind: "unavailable" };
  const blockers: CompatBlocker[] = [];
  const onAppleSilicon = state.host === "macos-aarch64";
  if (choice.via === "rosetta" || (choice.via === "wine" && onAppleSilicon)) {
    if (!state.rosettaInstalled) blockers.push({ kind: "needs_rosetta" });
    blockers.push({ kind: "rosetta_sunset", last_macos: "27" });
  }
  if (choice.via === "wine" && state.host === "macos-x86_64" && pkg.d3d12)
    blockers.push({ kind: "d3d12_unsupported_on_mac" });
  if (choice.via === "native") return { kind: "native" };
  if (choice.via === "rosetta") return { kind: "rosetta", blockers };
  return {
    kind: "compat",
    layer: choice.via === "proton" ? "proton" : "wine",
    status: pkg.profile?.status ?? "untested",
    notes: pkg.profile?.notes ?? null,
    protondb_tier: choice.via === "proton" ? pkg.protondb : null,
    blockers,
  };
}

function releaseFor(pkg: MockPackage, state: CatalogState): HostRelease | null {
  const choice = releaseChoice(pkg.platforms, state.host);
  if (!choice) return null;
  return {
    platform: choice.platform,
    version_id: mockId("0192a6f1-aaaa-7bbb-8ccc", Number.parseInt(pkg.id.slice(-6), 16)),
    version_label: pkg.version_label,
    sequence: pkg.sequence,
    total_size: pkg.total_size,
    published_at: pkg.updated_at,
    via: choice.via,
  };
}

const HARD_BLOCKERS = new Set(["d3d12_unsupported_on_mac", "needs_rosetta"]);

export function catalogHandlers(
  state: CatalogState &
    DownloadsState & {
      installs: InstalledPackage[];
      libraries: LibraryInfo[];
      servers: ServerProfile[];
    },
): Record<string, Handler> {
  const visible = () => state.packages.filter((p) => !state.removedPackages.includes(p.id));
  const find = (id: unknown): MockPackage => {
    const pkg = visible().find((p) => p.id === id);
    if (!pkg) fail({ kind: "not_found" } satisfies CatalogError);
    return pkg;
  };
  const itemOf = (pkg: MockPackage): CatalogItem => ({
    package_id: pkg.id,
    slug: pkg.slug,
    title: pkg.title,
    summary: pkg.summary,
    genres: pkg.genres,
    cover_url: null,
    platforms: pkg.platforms,
    availability: releaseChoice(pkg.platforms, state.host)?.via ?? "unavailable",
    updated_at: pkg.updated_at,
  });
  const plan = (id: unknown): InstallPlan => {
    const pkg = visible().find((p) => p.id === id);
    if (!pkg) fail({ kind: "not_found" } satisfies InstallPlanError);
    const release = releaseFor(pkg, state);
    if (!release) fail({ kind: "no_release" } satisfies InstallPlanError);
    if (state.installs.some((i) => i.package.package_id === pkg.id))
      fail({ kind: "already_installed" } satisfies InstallPlanError);
    const compat = compatFor(pkg, state);
    const blocker =
      compat.kind === "compat" || compat.kind === "rosetta"
        ? compat.blockers.find((b) => HARD_BLOCKERS.has(b.kind))
        : undefined;
    if (blocker) fail({ kind: "blocked", blocker } satisfies InstallPlanError);
    return {
      package_id: pkg.id,
      release,
      download_bytes: release.total_size,
      required_bytes: release.total_size + 64 * 1024 ** 2,
    };
  };

  return {
    catalog_list: (args) => {
      const query = args.query as CatalogQuery;
      const needle = query.query.trim().toLocaleLowerCase();
      const collator = new Intl.Collator("en", { sensitivity: "base", numeric: true });
      const matches = visible()
        .filter(
          (p) =>
            (needle === "" || p.title.toLocaleLowerCase().includes(needle)) &&
            (query.genre === null || p.genres.includes(query.genre)),
        )
        .sort((a, b) =>
          query.sort === "recent"
            ? b.updated_at.localeCompare(a.updated_at) || collator.compare(a.title, b.title)
            : collator.compare(a.title, b.title),
        );
      const start = query.cursor ? Number.parseInt(query.cursor, 10) : 0;
      const items = matches.slice(start, start + state.catalogPageSize).map(itemOf);
      const next = start + state.catalogPageSize;
      return { items, next_cursor: next < matches.length ? String(next) : null };
    },
    catalog_genres: () => {
      const counts = new Map<string, number>();
      for (const pkg of visible())
        for (const genre of pkg.genres) counts.set(genre, (counts.get(genre) ?? 0) + 1);
      return [...counts]
        .map(([genre, count]): GenreCount => ({ genre, count }))
        .sort((a, b) => a.genre.localeCompare(b.genre));
    },
    package_details: (args): PackageDetails => {
      const pkg = find(args.packageId);
      return {
        package_id: pkg.id,
        slug: pkg.slug,
        title: pkg.title,
        summary: pkg.summary,
        description: pkg.description,
        developer: pkg.developer,
        publisher: pkg.publisher,
        release_date: pkg.release_date,
        genres: pkg.genres,
        platforms: pkg.platforms,
        cover_url: null,
        hero_url: null,
        logo_url: null,
        screenshots: pkg.screenshots,
        release: releaseFor(pkg, state),
        compat: compatFor(pkg, state),
      };
    },
    install_plan: (args) => plan(args.packageId),
    install_start: (args) => {
      if (state.trustExpired) fail({ kind: "trust_expired" } satisfies InstallStartError);
      const planned = plan(args.packageId);
      const pkg = find(args.packageId);
      const library = state.libraries.find((l) => l.id === args.libraryId);
      if (!library) fail({ kind: "io", detail: "unknown library" } satisfies InstallStartError);
      if (!library.online)
        fail({ kind: "library_offline", library_path: library.path } satisfies InstallStartError);
      if (library.free_bytes !== null && library.free_bytes < planned.required_bytes)
        fail({
          kind: "insufficient_space",
          required_bytes: planned.required_bytes,
          available_bytes: library.free_bytes,
        } satisfies InstallStartError);
      const server = state.servers.find((s) => s.active);
      state.installs = [
        ...state.installs,
        {
          package: { server_id: server?.id ?? "", package_id: pkg.id },
          slug: pkg.slug,
          title: pkg.title,
          cover_url: null,
          library_id: library.id,
          install_path: `${library.path}/${pkg.slug}`,
          platform: planned.release.platform,
          version_label: planned.release.version_label,
          sequence: planned.release.sequence,
          size_bytes: planned.release.total_size,
          installed_at: null,
          last_played_at: null,
          playtime_seconds: 0,
          state: "installing",
          update: null,
          favorite: false,
          collection_ids: [],
          running: false,
          targets: [],
          compat:
            planned.release.via === "proton"
              ? "proton"
              : planned.release.via === "wine"
                ? "wine"
                : "native",
          cloud_saves: "unsupported",
        },
      ];
      queueInstall(
        state,
        makeJob(pkg, server?.id ?? "", library.id, {
          version_label: planned.release.version_label,
          bytes_total: planned.download_bytes,
          queued_at: new Date().toISOString(),
        }),
      );
      void events.installsChanged.emit({});
      return null;
    },
    rosetta_install: () => {
      state.rosettaInstalled = true;
      return null;
    },
  };
}
