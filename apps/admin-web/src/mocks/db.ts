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

export interface MockDb {
  /** `null` = not signed in (every authenticated call answers 401). */
  role: Role | null;
  csrf: string;
  users: Schemas["AdminUser"][];
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
  };
}

export function currentUser(db: MockDb): Schemas["User"] | null {
  if (!db.role) return null;
  const id = db.role === "owner" ? IDS.owner : db.role === "admin" ? IDS.admin : IDS.user;
  return db.users.find((u) => u.id === id) ?? null;
}
