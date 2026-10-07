// MSW handlers for compat profiles (09-compatibility §4) and the discovery document's server id.
import type { Schemas } from "@vgames/api-client";
import { HttpResponse, http } from "msw";
import { type MockDb, SERVER_ID } from "./db";
import { guard, problem } from "./handlers";

/** The same made-up fingerprint as the launcher mocks. */
const FINGERPRINT = "VG1-7K2M-Q9XD-4HT8-B3NW-RC5E-X1JP-V6GA-M0ZF";

export function compatHandlers(db: MockDb) {
  const latest = (pkg: string) => {
    const out: Record<string, Schemas["SignedCompatProfile"]> = {};
    for (const p of db.compat[pkg] ?? []) {
      const known = out[p.target];
      if (!known || p.revision > known.revision) out[p.target] = p;
    }
    return Object.values(out);
  };
  return [
    http.get("*/.well-known/vgames.json", () =>
      HttpResponse.json({
        format: "vgames.server/1",
        server_id: SERVER_ID,
        name: "Friday Night Games",
        api_versions: ["v1"],
        root_public_key: "AAAA",
        root_key_fingerprint: FINGERPRINT,
        features: ["admin_web"],
      }),
    ),

    http.get("*/v1/packages/:id/compat", ({ request, params }) => {
      const denied = guard(db, request, "user");
      if (denied) return denied;
      const pkg = db.packages.find((p) => p.id === params.id);
      if (pkg?.status !== "published") return problem(404, "not_found", "Not found");
      return HttpResponse.json({ items: latest(pkg.id) });
    }),

    http.get("*/v1/admin/packages/:id/compat/:target", ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const id = String(params.id);
      if (!db.packages.some((p) => p.id === id)) return problem(404, "not_found", "Not found");
      if (!["linux", "macos"].includes(String(params.target)))
        return problem(400, "invalid_path", "Unknown target");
      const q = new URL(request.url).searchParams;
      const after = q.get("cursor");
      const fixture = db.compatHistoryFixture;
      if (fixture && params.target === "linux") {
        fixture.cursors.push(after);
        if (fixture.forbidden) return problem(403, "forbidden", "Not allowed");
        return HttpResponse.json(fixture.pages[after ?? "first"] ?? { items: [] });
      }
      const limit = Number(q.get("limit") ?? 50);
      const rows = (db.compat[id] ?? [])
        .filter((p) => p.target === params.target && (!after || p.revision < Number(after)))
        .sort((a, b) => b.revision - a.revision);
      const items = rows.slice(0, limit);
      return HttpResponse.json({
        items,
        ...(rows.length > limit ? { next_cursor: String(items.at(-1)?.revision) } : {}),
      });
    }),

    http.put("*/v1/admin/packages/:id/compat/:target", async ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const id = String(params.id);
      const target = String(params.target) as "linux" | "macos";
      if (!db.packages.some((p) => p.id === id)) return problem(404, "not_found", "Not found");
      const body = (await request.json()) as {
        document: string;
        signature: Schemas["SignatureEnvelope"];
      };
      if (!db.trustedKeys[body.signature.key_id])
        return problem(422, "publisher_key_untrusted", "The key is not trusted");
      let doc: { target?: string; revision?: number; status?: string; runner?: { kind?: string } };
      try {
        doc = JSON.parse(atob(body.document));
      } catch {
        return problem(422, "compat_invalid", "Not a compat profile", { detail: "not JSON" });
      }
      if (doc.target !== target || (target === "linux") !== (doc.runner?.kind === "proton"))
        return problem(422, "compat_invalid", "Invalid profile", {
          detail: "runner.kind must be proton for linux and wine for macos",
        });
      const current = latest(id).find((p) => p.target === target);
      if (current && (doc.revision ?? 0) <= current.revision)
        return problem(409, "revision_conflict", "A newer revision exists");
      const stored: Schemas["SignedCompatProfile"] = {
        target,
        revision: doc.revision ?? 1,
        status: (doc.status ?? "untested") as Schemas["SignedCompatProfile"]["status"],
        document: body.document,
        signature: body.signature,
        created_at: new Date().toISOString(),
      };
      db.compat[id] = [...(db.compat[id] ?? []), stored];
      return HttpResponse.json(stored, { status: 201 });
    }),
  ];
}
