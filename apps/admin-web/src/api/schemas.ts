// zod schemas for every response the admin web reads. Unknown fields are stripped (the server may be
// newer); missing or mistyped fields fail loudly as `schema` errors.
//
// Each schema is checked at compile time against the generated OpenAPI types (`Schemas`): the
// parsed value must be assignable to the contract type, so drift in openapi.yaml breaks the build.
import type { Schemas } from "@vgames/api-client";
import { z } from "zod";

/** The contract type, with optional keys also accepting `undefined` (how zod types them). */
type Loose<T> = T extends readonly (infer U)[]
  ? Loose<U>[]
  : T extends object
    ? { [K in keyof T]: Loose<T[K]> | (object extends Pick<T, K> ? undefined : never) }
    : T;

/** Compile-time check that a zod schema's output fits a contract type. */
function contract<T>() {
  return <S extends z.ZodType<Loose<T>>>(schema: S): S => schema;
}

export const Uuid = z.uuid();
export const Timestamp = z.iso.datetime({ offset: true });
export const Role = z.enum(["user", "admin", "owner"]);

export const UserPublicSchema = contract<Schemas["UserPublic"]>()(
  z.object({
    id: Uuid,
    username: z.string(),
    display_name: z.string().optional(),
    avatar_url: z.url().optional(),
  }),
);

export const UserSchema = contract<Schemas["User"]>()(
  z.object({
    id: Uuid,
    discord_id: z
      .string()
      .regex(/^[0-9]{5,25}$/)
      .optional(),
    username: z.string(),
    display_name: z.string().optional(),
    avatar_url: z.url().optional(),
    role: Role,
    created_at: Timestamp,
  }),
);

export const SessionSchema = contract<Schemas["Session"]>()(
  z.object({
    id: Uuid,
    kind: z.enum(["desktop", "web"]),
    device_name: z.string().optional(),
    user_agent: z.string().optional(),
    created_at: Timestamp,
    last_used_at: Timestamp,
    current: z.boolean(),
  }),
);

export const MeSchema = contract<Schemas["Me"]>()(
  z.object({
    user: UserSchema,
    session: SessionSchema,
    device_id: Uuid.optional(),
    csrf_token: z.string().optional(),
  }),
);
export type Me = z.infer<typeof MeSchema>;

export const AuthStartResponseSchema = contract<Schemas["AuthStartResponse"]>()(
  z.object({ authorize_url: z.url(), expires_at: Timestamp }),
);

// ---------------------------------------------------------------- packages (A3-T14)

export const PackageStatusSchema = z.enum(["draft", "published", "hidden", "archived"]);
export type PackageStatus = z.infer<typeof PackageStatusSchema>;

export const PlatformSchema = z.enum([
  "windows-x86_64",
  "windows-aarch64",
  "linux-x86_64",
  "linux-aarch64",
  "macos-aarch64",
  "macos-x86_64",
]);

export const AssetSchema = contract<Schemas["Asset"]>()(
  z.object({
    id: Uuid,
    kind: z.enum(["cover", "hero", "logo", "screenshot", "icon"]),
    url: z.string(),
    width: z.number().int(),
    height: z.number().int(),
    content_type: z.enum(["image/jpeg", "image/png", "image/webp"]),
    source: z.enum(["igdb", "steam", "upload"]).optional(),
  }),
);
export type Asset = z.infer<typeof AssetSchema>;

export const JobStateSchema = z.enum(["queued", "running", "succeeded", "failed", "dead"]);

export const JobSchema = contract<Schemas["Job"]>()(
  z.object({
    id: Uuid,
    kind: z.string(),
    state: JobStateSchema,
    attempts: z.number().int().min(0),
    max_attempts: z.number().int().min(1),
    last_error: z.string().optional(),
    run_at: Timestamp.optional(),
    created_at: Timestamp,
    finished_at: Timestamp.optional(),
  }),
);
export type Job = z.infer<typeof JobSchema>;

export const FieldSourceSchema = z.enum(["admin", "igdb", "steam"]);
export type FieldSource = z.infer<typeof FieldSourceSchema>;

export const AdminPackageSchema = contract<Schemas["AdminPackage"]>()(
  z.object({
    id: Uuid,
    slug: z.string(),
    title: z.string(),
    summary: z.string().optional(),
    genres: z.array(z.string()).optional(),
    cover: AssetSchema.optional(),
    platforms: z.array(PlatformSchema),
    updated_at: Timestamp,
    description: z.string().optional(),
    developer: z.string().optional(),
    publisher: z.string().optional(),
    release_date: z.iso.date().optional(),
    protondb_tier: z.enum(["platinum", "gold", "silver", "bronze", "borked", "pending"]).optional(),
    hero: AssetSchema.optional(),
    logo: AssetSchema.optional(),
    screenshots: z.array(AssetSchema).optional(),
    releases: z
      .array(
        z.object({
          platform: PlatformSchema,
          version_id: Uuid,
          version_label: z.string(),
          sequence: z.number().int().min(1),
          total_size: z.number().int().min(0),
          published_at: Timestamp,
        }),
      )
      .optional(),
    status: PackageStatusSchema,
    steam_app_id: z.number().int().min(1).optional(),
    igdb_id: z.number().int().min(1).optional(),
    field_sources: z.record(z.string(), FieldSourceSchema),
    created_at: Timestamp,
    created_by: UserPublicSchema,
    metadata_job: JobSchema.optional(),
  }),
);
export type AdminPackage = z.infer<typeof AdminPackageSchema>;

export const AdminPackagePageSchema = contract<Schemas["AdminPackagePage"]>()(
  z.object({ items: z.array(AdminPackageSchema), next_cursor: z.string().optional() }),
);

// ---------------------------------------------------------------- metadata (A3-T15)

export const MetadataCandidateSchema = contract<Schemas["MetadataCandidate"]>()(
  z.object({
    source: z.enum(["igdb", "steam"]),
    external_id: z.number().int().min(1),
    title: z.string(),
    release_year: z.number().int().optional(),
    score: z.number().min(0).max(1),
    data: z.object({
      title: z.string().optional(),
      summary: z.string().optional(),
      description: z.string().optional(),
      release_date: z.iso.date().optional(),
      developer: z.string().optional(),
      publisher: z.string().optional(),
      genres: z.array(z.string()).optional(),
      images: z
        .object({
          cover: z.url().optional(),
          hero: z.url().optional(),
          logo: z.url().optional(),
          screenshots: z.array(z.url()).optional(),
        })
        .optional(),
      external: z
        .object({
          steam_app_id: z.number().int().optional(),
          igdb_id: z.number().int().optional(),
          umu_id: z.string().optional(),
        })
        .optional(),
    }),
    fetched_at: Timestamp,
  }),
);
export type MetadataCandidate = z.infer<typeof MetadataCandidateSchema>;

export const CandidatesSchema = z.object({
  items: z.array(MetadataCandidateSchema),
  job: JobSchema.optional(),
});

// ---------------------------------------------------------------- versions (A3-T16)

export const VersionStateSchema = z.enum([
  "uploading",
  "verifying",
  "ready",
  "published",
  "failed",
  "yanked",
  "aborted",
]);
export type VersionState = z.infer<typeof VersionStateSchema>;
export type Platform = z.infer<typeof PlatformSchema>;

export const VersionSchema = contract<Schemas["Version"]>()(
  z.object({
    id: Uuid,
    package_id: Uuid,
    server_id: Uuid,
    platform: PlatformSchema,
    sequence: z.number().int().min(1),
    version_label: z.string(),
    state: VersionStateSchema,
    is_current_release: z.boolean().optional(),
    failure_reason: z.string().optional(),
    total_size: z.number().int().min(0).optional(),
    file_count: z.number().int().min(0).optional(),
    chunk_count: z.number().int().min(0).optional(),
    pack_count: z.number().int().min(1).optional(),
    publisher_key_id: z.string().optional(),
    verify_progress: z.number().min(0).max(1).optional(),
    created_at: Timestamp,
    created_by: UserPublicSchema,
    finalized_at: Timestamp.optional(),
    verified_at: Timestamp.optional(),
    published_at: Timestamp.optional(),
    yanked_at: Timestamp.optional(),
  }),
);
export type Version = z.infer<typeof VersionSchema>;

export const VersionPageSchema = contract<Schemas["VersionPage"]>()(
  z.object({ items: z.array(VersionSchema), next_cursor: z.string().optional() }),
);

export const UploadTargetSchema = contract<Schemas["UploadTarget"]>()(
  z.object({
    url: z.url(),
    method: z.enum(["POST", "PUT"]),
    headers: z.record(z.string(), z.string()),
    expires_at: Timestamp,
  }),
);
export type UploadTarget = z.infer<typeof UploadTargetSchema>;
