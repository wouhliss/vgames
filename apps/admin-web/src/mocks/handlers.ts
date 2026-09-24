// MSW handlers implementing the admin endpoints from openapi/openapi.yaml over an in-memory db.
// The same handlers power `vite --mode mock`, the Vitest suites and the Playwright MSW suite.
import type { Schemas } from "@vgames/api-client";
import { HttpResponse, http } from "msw";
import { currentUser, IDS, type MockDb } from "./db";

type Problem = Schemas["Problem"];

export function problem(status: number, code: string, title: string, extra: Partial<Problem> = {}) {
  const body: Problem = {
    type: `urn:vgames:problem:${code}`,
    title,
    status,
    code,
    request_id: "01J-mock",
    ...extra,
  };
  return HttpResponse.json(body, {
    status,
    headers: { "Content-Type": "application/problem+json", "X-Request-Id": "01J-mock" },
  });
}

const UNSAFE = new Set(["POST", "PUT", "PATCH", "DELETE"]);

/** Auth + CSRF checks shared by every authenticated handler. Returns a problem response or null. */
export function guard(db: MockDb, request: Request, minRole: "user" | "admin" | "owner" = "admin") {
  const user = currentUser(db);
  if (!user) return problem(401, "unauthenticated", "Sign in required");
  if (UNSAFE.has(request.method) && request.headers.get("X-CSRF-Token") !== db.csrf) {
    return problem(403, "csrf_failed", "Missing or invalid CSRF token");
  }
  const rank = { user: 0, admin: 1, owner: 2 } as const;
  if (rank[user.role] < rank[minRole]) return problem(403, "forbidden", "Not allowed");
  return null;
}

export function createHandlers(db: MockDb) {
  return [
    http.get("*/v1/me", ({ request }) => {
      const denied = guard(db, request, "user");
      if (denied) return denied;
      const user = currentUser(db);
      const me: Schemas["Me"] = {
        user: user as Schemas["User"],
        session: {
          id: IDS.session,
          kind: "web",
          created_at: "2026-09-24T10:00:00Z",
          last_used_at: "2026-09-24T10:00:00Z",
          current: true,
        },
        csrf_token: db.csrf,
      };
      return HttpResponse.json(me);
    }),

    http.post("*/v1/auth/discord/start", async ({ request }) => {
      const body = (await request.json()) as Schemas["AuthStartRequest"];
      if (
        body.client !== "web" ||
        (body.return_to && !/^\/admin(\/[A-Za-z0-9._~/-]*)?$/.test(body.return_to))
      ) {
        return problem(400, "validation_failed", "Invalid request", {
          errors: [{ field: "return_to", code: "pattern" }],
        });
      }
      const res: Schemas["AuthStartResponse"] = {
        authorize_url: `https://discord.example/oauth2/authorize?state=mock&return_to=${encodeURIComponent(body.return_to ?? "/admin/")}`,
        expires_at: "2026-09-24T10:10:00Z",
      };
      return HttpResponse.json(res);
    }),

    http.post("*/v1/auth/logout", ({ request }) => {
      const denied = guard(db, request, "user");
      if (denied) return denied;
      db.role = null;
      return new HttpResponse(null, { status: 204 });
    }),
  ];
}
