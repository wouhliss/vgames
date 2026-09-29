// MSW handlers for /v1/admin/packages (openapi.yaml, admin-packages): list with filters and cursor,
// create with Idempotency-Key, read with ETag, JSON Merge Patch with If-Match, soft delete.
// Validation mirrors apps/api/src/packages.rs so the UI sees the same field errors.
import type { Schemas } from "@vgames/api-client";
import { HttpResponse, http } from "msw";
import { type AdminPackage, type MockDb, makePackage, packageId } from "./db";
import { guard, problem } from "./handlers";

type FieldError = { field: string; code: string; message: string };

const SLUG = /^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/;
const chars = (s: string) => [...s].length;

function slugify(title: string): string {
  let out = "";
  let dash = false;
  for (const c of title.normalize("NFKD")) {
    if (/^[A-Za-z0-9]$/.test(c)) {
      out += c.toLowerCase();
      dash = false;
    } else if (!/^\p{M}$/u.test(c) && out.length > 0 && !dash) {
      out += "-";
      dash = true;
    }
  }
  const s = out
    .replace(/^-+|-+$/g, "")
    .slice(0, 56)
    .replace(/^-+|-+$/g, "");
  return s || "package";
}

export const etagOf = (db: MockDb, id: string) => `W/"${db.etags[id] ?? 1}"`;

/** Simulates another admin saving the package (tests): applies `patch` and bumps the ETag. */
export function editElsewhere(db: MockDb, id: string, patch: Partial<AdminPackage>): void {
  db.packages = db.packages.map((p) =>
    p.id === id ? { ...p, ...patch, updated_at: new Date().toISOString() } : p,
  );
  db.etags[id] = (db.etags[id] ?? 1) + 1;
}

function badRequest(errors: FieldError[]) {
  return problem(400, "validation_failed", "Invalid request", { errors });
}

function checkText(errors: FieldError[], field: string, v: string, min: number, max: number) {
  const n = chars(v.trim());
  if (n < min || n > max)
    errors.push({ field, code: "length", message: `must be ${min}-${max} characters` });
}

function validatePatch(body: Schemas["AdminPackagePatch"]): FieldError[] {
  const errors: FieldError[] = [];
  if (body.title !== undefined) checkText(errors, "title", body.title, 1, 200);
  if (body.slug !== undefined && !SLUG.test(body.slug))
    errors.push({
      field: "slug",
      code: "pattern",
      message: "must be lowercase letters, digits and dashes (1-64)",
    });
  for (const [field, max] of [
    ["summary", 500],
    ["description", 20_000],
    ["developer", 200],
    ["publisher", 200],
  ] as const) {
    const v = body[field];
    if (typeof v === "string" && chars(v) > max)
      errors.push({ field, code: "max_length", message: `must be at most ${max} characters` });
  }
  if (body.genres) {
    if (body.genres.length > 20)
      errors.push({ field: "genres", code: "max_items", message: "at most 20 genres" });
    body.genres.forEach((g, i) => {
      const n = chars(g.trim());
      if (n < 1 || n > 64)
        errors.push({ field: `genres[${i}]`, code: "length", message: "must be 1-64 characters" });
    });
  }
  for (const field of ["steam_app_id", "igdb_id"] as const) {
    const v = body[field];
    if (typeof v === "number" && (!Number.isInteger(v) || v < 1))
      errors.push({ field, code: "minimum", message: "must be at least 1" });
  }
  if (body.release_date && !/^\d{4}-\d{2}-\d{2}$/.test(body.release_date))
    errors.push({ field: "release_date", code: "format", message: "must be a date" });
  return errors;
}

export function packageHandlers(db: MockDb) {
  const find = (id: string) => db.packages.find((p) => p.id === id);
  const json = (db_: MockDb, pkg: AdminPackage, status = 200) =>
    HttpResponse.json(pkg, { status, headers: { ETag: etagOf(db_, pkg.id) } });
  let created = 100;

  return [
    http.get("*/v1/admin/packages", ({ request }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const url = new URL(request.url);
      const q = (url.searchParams.get("q") ?? "").toLowerCase();
      const status = url.searchParams.get("status");
      const limit = Math.min(200, Math.max(1, Number(url.searchParams.get("limit") ?? 50)));
      const cursor = url.searchParams.get("cursor");
      if (chars(q) > 100)
        return badRequest([{ field: "q", code: "max_length", message: "at most 100 characters" }]);
      const offset = cursor ? Number(cursor.replace(/^o/, "")) : 0;
      if (!Number.isInteger(offset) || offset < 0)
        return problem(400, "invalid_cursor", "Bad cursor");
      const matching = db.packages
        .filter((p) => !status || p.status === status)
        .filter((p) => !q || p.title.toLowerCase().includes(q) || p.slug.includes(q))
        .sort((a, b) => a.title.localeCompare(b.title));
      const items = matching.slice(offset, offset + limit);
      const next = offset + limit < matching.length ? `o${offset + limit}` : undefined;
      const page: Schemas["AdminPackagePage"] = next ? { items, next_cursor: next } : { items };
      return HttpResponse.json(page);
    }),

    http.post("*/v1/admin/packages", async ({ request }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const key = request.headers.get("Idempotency-Key");
      if (key && !/^[A-Za-z0-9_-]{16,128}$/.test(key))
        return problem(400, "invalid_idempotency_key", "Bad Idempotency-Key");
      const body = (await request.json()) as Schemas["AdminPackageCreate"];
      if (key && db.idempotency[key]) {
        const existing = find(db.idempotency[key]);
        if (existing) return json(db, existing, 201);
      }
      const errors: FieldError[] = [];
      checkText(errors, "title", body.title ?? "", 1, 200);
      if (body.slug !== undefined && !SLUG.test(body.slug))
        errors.push({
          field: "slug",
          code: "pattern",
          message: "must be lowercase letters, digits and dashes (1-64)",
        });
      for (const field of ["steam_app_id", "igdb_id"] as const) {
        const v = body[field];
        if (v !== undefined && (!Number.isInteger(v) || v < 1))
          errors.push({ field, code: "minimum", message: "must be at least 1" });
      }
      if (errors.length > 0) return badRequest(errors);
      let slug = body.slug ?? slugify(body.title);
      if (db.packages.some((p) => p.slug === slug)) {
        if (body.slug) return problem(409, "slug_taken", "Another package already uses this slug");
        let n = 2;
        while (db.packages.some((p) => p.slug === `${slug}-${n}`)) n += 1;
        slug = `${slug}-${n}`;
      }
      created += 1;
      const pkg = makePackage(created, {
        id: packageId(created),
        slug,
        title: body.title.trim(),
        ...(body.steam_app_id ? { steam_app_id: body.steam_app_id } : {}),
        ...(body.igdb_id ? { igdb_id: body.igdb_id } : {}),
        field_sources: { title: "admin" },
        created_at: new Date().toISOString(),
        updated_at: new Date().toISOString(),
      });
      db.packages = [...db.packages, pkg];
      db.etags[pkg.id] = 1;
      if (key) db.idempotency[key] = pkg.id;
      return json(db, pkg, 201);
    }),

    http.get("*/v1/admin/packages/:id", ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const pkg = find(String(params.id));
      if (!pkg) return problem(404, "not_found", "Not found");
      return json(db, pkg);
    }),

    http.patch("*/v1/admin/packages/:id", async ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const pkg = find(String(params.id));
      if (!pkg) return problem(404, "not_found", "Not found");
      const ifMatch = request.headers.get("If-Match");
      if (!ifMatch) return problem(428, "precondition_required", "If-Match is required");
      if (ifMatch !== etagOf(db, pkg.id))
        return problem(412, "precondition_failed", "The package changed since you loaded it");
      if (!(request.headers.get("Content-Type") ?? "").startsWith("application/merge-patch+json"))
        return problem(415, "unsupported_media_type", "Use application/merge-patch+json");
      const body = (await request.json()) as Schemas["AdminPackagePatch"];
      const errors = validatePatch(body);
      if (errors.length > 0) return badRequest(errors);
      if (body.slug && body.slug !== pkg.slug && db.packages.some((p) => p.slug === body.slug))
        return problem(409, "slug_taken", "Another package already uses this slug");
      const next: AdminPackage = { ...pkg, field_sources: { ...pkg.field_sources } };
      const record = next as unknown as Record<string, unknown>;
      for (const [key, value] of Object.entries(body)) {
        const slot = /^(cover|hero|logo)_asset_id$/.exec(key)?.[1] as
          | "cover"
          | "hero"
          | "logo"
          | undefined;
        if (slot) {
          const asset = value === null ? undefined : db.uploadedAssets?.[String(value)];
          if (value !== null && !asset)
            return badRequest([{ field: key, code: "unknown_asset", message: "no such image" }]);
          if (asset) next[slot] = { ...asset, kind: slot };
          else delete next[slot];
        } else if (value === null) delete record[key];
        else record[key] = value;
        if (key !== "status") next.field_sources[slot ?? key] = "admin";
      }
      next.updated_at = new Date().toISOString();
      db.packages = db.packages.map((p) => (p.id === pkg.id ? next : p));
      db.etags[pkg.id] = (db.etags[pkg.id] ?? 1) + 1;
      return json(db, next);
    }),

    http.delete("*/v1/admin/packages/:id", ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const pkg = find(String(params.id));
      if (!pkg) return problem(404, "not_found", "Not found");
      const ifMatch = request.headers.get("If-Match");
      if (!ifMatch) return problem(428, "precondition_required", "If-Match is required");
      if (ifMatch !== etagOf(db, pkg.id))
        return problem(412, "precondition_failed", "The package changed since you loaded it");
      db.packages = db.packages.filter((p) => p.id !== pkg.id);
      return new HttpResponse(null, { status: 204 });
    }),
  ];
}
