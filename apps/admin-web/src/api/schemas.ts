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
