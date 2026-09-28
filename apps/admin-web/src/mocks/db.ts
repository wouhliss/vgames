// In-memory server state for MSW handlers. Fixtures are typed with the generated OpenAPI types, so
// they cannot drift from the contract.
import type { Schemas } from "@vgames/api-client";

export type Role = Schemas["Role"];

export const IDS = {
  owner: "01920000-0000-7000-8000-00000000a001",
  admin: "01920000-0000-7000-8000-00000000a002",
  user: "01920000-0000-7000-8000-00000000a003",
  session: "01920000-0000-7000-8000-00000000c001",
} as const;

export type AdminPackage = Schemas["AdminPackage"];

export interface MockDb {
  /** `null` = not signed in (every authenticated call answers 401). */
  role: Role | null;
  csrf: string;
  users: Schemas["AdminUser"][];
  packages: AdminPackage[];
  /** Current ETag per package id (bumped on every write). */
  etags: Record<string, number>;
  /** Package created per Idempotency-Key. */
  idempotency: Record<string, string>;
  /** Metadata candidates per package id. */
  candidates: Record<string, Schemas["MetadataCandidate"][]>;
  /** The latest metadata job per package id. */
  metadataJobs: Record<string, Schemas["Job"]>;
  /** How the next lookup for a package ends (default: succeeds with the demo candidates). */
  lookupOutcome: Record<string, "succeed" | "fail" | "empty">;
  /** Asset ids whose image can't be served (broken-image placeholder). */
  brokenAssets: string[];
  /** Images uploaded but not (yet) used by a package. */
  uploadedAssets?: Record<string, Schemas["Asset"]>;
}

export function createDb(role: Role | null = "admin"): MockDb {
  const t = "2026-09-24T10:00:00Z";
  return {
    role,
    csrf: "mock-csrf-token-0123456789",
    users: [
      {
        id: IDS.owner,
        username: "olive",
        display_name: "Olive",
        role: "owner",
        created_at: t,
        discord_id: "100000000000000001",
      },
      {
        id: IDS.admin,
        username: "adrian",
        display_name: "Adrian",
        role: "admin",
        created_at: t,
        discord_id: "100000000000000002",
      },
      {
        id: IDS.user,
        username: "sam",
        role: "user",
        created_at: t,
        discord_id: "100000000000000003",
      },
    ],
    packages: makePackages(),
    etags: {},
    idempotency: {},
    candidates: { [packageId(1)]: demoCandidates() },
    metadataJobs: {
      [packageId(1)]: {
        id: "01920000-0000-7000-8000-0000000d0001",
        kind: "metadata.fetch",
        state: "succeeded",
        attempts: 1,
        max_attempts: 5,
        created_at: t,
        finished_at: t,
      },
    },
    lookupOutcome: {},
    brokenAssets: [assetId(3)],
  };
}

export const assetId = (n: number) =>
  `01920000-0000-7000-8000-0000000a${n.toString(16).padStart(4, "0")}`;

export function makeAsset(
  n: number,
  kind: Schemas["Asset"]["kind"],
  source: "igdb" | "steam" | "upload" = "igdb",
): Schemas["Asset"] {
  return {
    id: assetId(n),
    kind,
    url: `/v1/assets/${assetId(n)}`,
    width: kind === "cover" ? 600 : 1280,
    height: kind === "cover" ? 900 : 720,
    content_type: "image/png",
    source,
  };
}

export function demoCandidates(): Schemas["MetadataCandidate"][] {
  const fetched = "2026-09-24T10:05:00Z";
  return [
    {
      source: "igdb",
      external_id: 1942,
      title: "Hollow Harbor",
      release_year: 2024,
      score: 0.97,
      data: {
        title: "Hollow Harbor",
        summary: "A fishing town hides an old secret beneath the tide.",
        description: "Fish, trade, explore and uncover what sleeps in the harbor.",
        release_date: "2024-03-15",
        developer: "Tidewater Games",
        publisher: "Tidewater Publishing",
        genres: ["Adventure", "Simulation", "Indie"],
        images: {
          cover: "https://images.igdb.example/cover.jpg",
          screenshots: ["https://images.igdb.example/1.jpg", "https://images.igdb.example/2.jpg"],
        },
        external: { igdb_id: 1942, steam_app_id: 480 },
      },
      fetched_at: fetched,
    },
    {
      source: "steam",
      external_id: 480,
      title: "Hollow Harbor: Deluxe",
      release_year: 2024,
      score: 0.81,
      data: {
        title: "Hollow Harbor: Deluxe",
        summary: "The deluxe edition.",
        genres: ["Adventure"],
        external: { steam_app_id: 480, umu_id: "umu-480" },
      },
      fetched_at: fetched,
    },
  ];
}

export const packageId = (n: number) =>
  `01920000-0000-7000-8000-0000000b${n.toString(16).padStart(4, "0")}`;

const OWNER_PUBLIC = { id: IDS.owner, username: "olive", display_name: "Olive" };

export function makePackage(n: number, patch: Partial<AdminPackage> = {}): AdminPackage {
  return {
    id: packageId(n),
    slug: `package-${n}`,
    title: `Package ${n}`,
    platforms: [],
    updated_at: "2026-09-24T10:00:00Z",
    status: "draft",
    field_sources: {},
    created_at: "2026-09-20T10:00:00Z",
    created_by: OWNER_PUBLIC,
    ...patch,
  };
}

function makePackages(): AdminPackage[] {
  return [
    makePackage(1, {
      slug: "hollow-harbor",
      title: "Hollow Harbor",
      status: "published",
      summary: "A cozy fishing town with a secret.",
      description: "## About\n\nFish, trade and explore.\n\n<script>alert(1)</script>",
      developer: "Tidewater Games",
      publisher: "Tidewater Games",
      release_date: "2024-03-14",
      genres: ["Adventure", "Simulation"],
      steam_app_id: 480,
      igdb_id: 1942,
      platforms: ["windows-x86_64", "linux-x86_64"],
      cover: makeAsset(1, "cover"),
      hero: makeAsset(2, "hero"),
      screenshots: [makeAsset(3, "screenshot"), makeAsset(4, "screenshot")],
      field_sources: {
        title: "igdb",
        summary: "igdb",
        description: "igdb",
        developer: "igdb",
        genres: "admin",
      },
    }),
    makePackage(2, {
      slug: "night-canyon",
      title: "Night Canyon",
      status: "published",
      genres: ["Racing"],
      platforms: ["windows-x86_64"],
      field_sources: { title: "steam" },
    }),
    makePackage(3, { slug: "paper-station", title: "Paper Station", status: "hidden" }),
    makePackage(4, { slug: "iron-garden", title: "Iron Garden", status: "archived" }),
    makePackage(5, { slug: "velvet-forge", title: "Velvet Forge" }),
    makePackage(6, {
      slug: "mirage-engine",
      title: "مرآة المحرك",
      summary: "عنوان من اليمين إلى اليسار",
    }),
    makePackage(7, { slug: "emoji-quest", title: "Emoji Quest 🎮🚀" }),
  ];
}

/** `count` generated packages (large-list tests). */
export function manyPackages(count: number): AdminPackage[] {
  return Array.from({ length: count }, (_, i) =>
    makePackage(1000 + i, {
      slug: `bulk-${i}`,
      title: `Bulk package ${String(i).padStart(5, "0")}`,
      status: i % 3 === 0 ? "published" : "draft",
    }),
  );
}

export function currentUser(db: MockDb): Schemas["User"] | null {
  if (!db.role) return null;
  const id = db.role === "owner" ? IDS.owner : db.role === "admin" ? IDS.admin : IDS.user;
  return db.users.find((u) => u.id === id) ?? null;
}
