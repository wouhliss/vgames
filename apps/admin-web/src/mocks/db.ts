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
  };
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
