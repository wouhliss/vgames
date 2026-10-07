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
  /** Versions per package id, newest first. */
  versions: Record<string, Schemas["Version"][]>;
  /** Emulated GCS: bytes received per `<version>/<pack>` (and the total once known). */
  gcs: Record<string, { received: number; total: number | null; complete: boolean }>;
  /** Emulated GCS: uploaded manifests per version id. */
  manifests: Record<string, { size: number }>;
  /** Publisher key ids in the trust bundle, with their holder. */
  trustedKeys: Record<string, string>;
  /** How verification of a version ends ("ok" by default). */
  verifyOutcome: Record<string, "ok" | "fail">;
  /** Test switches: the next N upload-session start URLs are already expired; GCS answers 503 N times. */
  faults: {
    expiredStarts: number;
    gcsErrors: number;
    networkDown?: boolean;
    /** The network goes down after this many more accepted pieces. */
    downAfterPieces?: number;
  };
  /** Published compat profiles per package id (all revisions). */
  compat: Record<string, Schemas["SignedCompatProfile"][]>;
  /** Browser history tests inject pages and failures through the mock server. */
  compatHistoryFixture?: {
    pages: Record<string, Schemas["CompatProfilePage"]>;
    cursors: (string | null)[];
    forbidden?: boolean;
  };
  allowlist: Schemas["AllowlistEntry"][];
  settings: Schemas["ServerSettings"];
  settingsEtag: number;
  trust: { version: number; expiresAt: string; keys: Schemas["PublisherKey"][] };
  jobs: Schemas["Job"][];
  audit: Schemas["AuditEntry"][];
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
        last_seen_at: t,
      },
      {
        id: "01920000-0000-7000-8000-00000000a004",
        username: "rtl_user",
        display_name: "مستخدم",
        role: "user",
        created_at: t,
        discord_id: "100000000000000004",
      },
      {
        id: "01920000-0000-7000-8000-00000000a005",
        username: "gone",
        role: "user",
        created_at: t,
        discord_id: "100000000000000005",
        disabled_at: t,
        disabled_reason: "Asked to leave",
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
    versions: { [packageId(1)]: demoVersions() },
    gcs: {},
    manifests: {},
    trustedKeys: { [MOCK_KEY_ID]: IDS.admin, [OTHER_KEY_ID]: IDS.owner },
    verifyOutcome: {},
    faults: { expiredStarts: 0, gcsErrors: 0 },
    compat: {},
    allowlist: [
      { discord_id: "200000000000000001", note: "Kim from the Friday group", created_at: t },
      { discord_id: "200000000000000002", created_at: t },
    ],
    settings: { registration_mode: "allowlist", name: "Friday Night Games", motd: "Be nice." },
    settingsEtag: 1,
    trust: makeTrust(),
    jobs: makeJobs(),
    audit: makeAudit(),
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

/** The mock publisher key (see mocks/keyfile.ts), trusted and held by the admin. */
export const MOCK_KEY_ID = "5a1ddc0a5e2e4a3c9f1b7d2e8c4a6b10";
/** A trusted key held by the owner (finalizing with it as the admin is refused). */
export const OTHER_KEY_ID = "0b7a1c3d5e7f90a1b2c3d4e5f6a7b8c9";
export const SERVER_ID = "01920000-0000-7000-8000-000000000001";

export const versionId = (n: number) =>
  `01920000-0000-7000-8000-0000000e${n.toString(16).padStart(4, "0")}`;

function demoVersions(): Schemas["Version"][] {
  const base = {
    package_id: `01920000-0000-7000-8000-0000000b0001`,
    server_id: SERVER_ID,
    created_by: { id: IDS.admin, username: "adrian", display_name: "Adrian" },
  };
  return [
    {
      ...base,
      id: versionId(3),
      platform: "linux-x86_64",
      sequence: 3,
      version_label: "1.2.0",
      state: "ready",
      total_size: 1_234_567_890,
      file_count: 1432,
      pack_count: 5,
      publisher_key_id: MOCK_KEY_ID,
      verify_progress: 1,
      created_at: "2026-09-24T09:00:00Z",
      finalized_at: "2026-09-24T09:30:00Z",
      verified_at: "2026-09-24T09:40:00Z",
    },
    {
      ...base,
      id: versionId(2),
      platform: "windows-x86_64",
      sequence: 2,
      version_label: "1.1.0",
      state: "published",
      is_current_release: true,
      total_size: 1_100_000_000,
      file_count: 1400,
      pack_count: 5,
      publisher_key_id: MOCK_KEY_ID,
      created_at: "2026-09-10T09:00:00Z",
      published_at: "2026-09-10T10:00:00Z",
    },
    {
      ...base,
      id: versionId(1),
      platform: "windows-x86_64",
      sequence: 1,
      version_label: "1.0.0",
      state: "yanked",
      total_size: 1_000_000_000,
      created_at: "2026-09-01T09:00:00Z",
      yanked_at: "2026-09-10T10:00:00Z",
    },
  ];
}

const day = 86_400_000;
const iso = (ms: number) => new Date(ms).toISOString();

function makeTrust(): MockDb["trust"] {
  const now = Date.now();
  const holder = (id: string, username: string) => ({ id, username });
  return {
    version: 7,
    expiresAt: iso(now + 200 * day),
    keys: [
      {
        key_id: MOCK_KEY_ID,
        public_key: "AAAA",
        holder: holder(IDS.admin, "adrian"),
        label: "Adrian's laptop",
        not_before: iso(now - 100 * day),
        not_after: iso(now + 300 * day),
      },
      {
        key_id: OTHER_KEY_ID,
        public_key: "BBBB",
        holder: holder(IDS.owner, "olive"),
        label: "Olive's desktop",
        not_before: iso(now - 300 * day),
        not_after: iso(now + 20 * day),
      },
      {
        key_id: "9f8e7d6c5b4a39281706f5e4d3c2b1a0",
        public_key: "CCCC",
        holder: holder(IDS.admin, "adrian"),
        label: "Old build server",
        not_before: iso(now - 400 * day),
        not_after: iso(now + 100 * day),
        revoked_at: iso(now - 10 * day),
        revocation_reason: "Machine was decommissioned",
      },
    ],
  };
}

function makeJobs(): Schemas["Job"][] {
  const kinds = ["metadata.fetch", "version.verify", "social.sweep", "assets.fetch"];
  const states: Schemas["Job"]["state"][] = ["succeeded", "running", "queued", "failed", "dead"];
  return Array.from({ length: 12 }, (_, i) => ({
    id: `01920000-0000-7000-8000-0000000f${i.toString(16).padStart(4, "0")}`,
    kind: kinds[i % kinds.length] ?? "metadata.fetch",
    state: states[i % states.length] ?? "succeeded",
    attempts: i % 5 === 4 ? 5 : 1,
    max_attempts: 5,
    ...(i % 5 >= 3 ? { last_error: "IGDB answered 503 <Service Unavailable>" } : {}),
    created_at: iso(Date.parse("2026-09-24T10:00:00Z") + i * 60_000),
  }));
}

function makeAudit(): Schemas["AuditEntry"][] {
  const actions = [
    "package.update",
    "version.publish",
    "user.disable",
    "allowlist.add",
    "settings.update",
  ];
  return Array.from({ length: 130 }, (_, i) => ({
    id: `01920000-0000-7000-8000-0000000c${i.toString(16).padStart(4, "0")}`,
    actor: { id: i % 2 ? IDS.admin : IDS.owner, username: i % 2 ? "adrian" : "olive" },
    action: actions[i % actions.length] ?? "package.update",
    target_type: "package",
    target_id: packageId(1 + (i % 3)),
    ip: "192.0.2.10",
    details: { field: "summary", note: `<b>entry ${i}</b>` } as never,
    created_at: iso(Date.parse("2026-09-28T10:00:00Z") - i * 3_600_000),
  }));
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
