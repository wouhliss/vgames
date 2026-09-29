// MSW handlers for metadata review and images (openapi.yaml, admin-packages): the lookup job moves
// queued → running → a terminal state as the UI polls; candidates; apply with If-Match; image upload
// (multipart, ≤ 10 MiB, JPEG/PNG/WebP), delete, and serving images.
import type { Schemas } from "@vgames/api-client";
import { HttpResponse, http } from "msw";
import { assetId, demoCandidates, type MockDb } from "./db";
import { guard, problem } from "./handlers";
import { etagOf } from "./packages";

const PNG_1X1 = Uint8Array.from(
  atob(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mN8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==",
  ),
  (c) => c.charCodeAt(0),
);

export function metadataHandlers(db: MockDb) {
  const find = (id: string) => db.packages.find((p) => p.id === id);
  let jobs = 0;
  let assets = 100;

  /** Each poll moves an active job one step on. */
  function advance(id: string): Schemas["Job"] | undefined {
    const job = db.metadataJobs[id];
    if (!job) return undefined;
    if (job.state === "queued") db.metadataJobs[id] = { ...job, state: "running", attempts: 1 };
    else if (job.state === "running") {
      const outcome = db.lookupOutcome[id] ?? "succeed";
      const now = new Date().toISOString();
      if (outcome === "fail") {
        db.metadataJobs[id] = {
          ...job,
          state: "dead",
          attempts: job.max_attempts,
          last_error: "IGDB answered 503 Service Unavailable",
          finished_at: now,
        };
      } else {
        db.metadataJobs[id] = { ...job, state: "succeeded", finished_at: now };
        db.candidates[id] = outcome === "empty" ? [] : demoCandidates();
      }
    }
    return db.metadataJobs[id];
  }

  return [
    http.get("*/v1/admin/packages/:id/metadata/candidates", ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const id = String(params.id);
      if (!find(id)) return problem(404, "not_found", "Not found");
      const job = advance(id);
      return HttpResponse.json({ items: db.candidates[id] ?? [], ...(job ? { job } : {}) });
    }),

    http.post("*/v1/admin/packages/:id/metadata/refresh", ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const id = String(params.id);
      if (!find(id)) return problem(404, "not_found", "Not found");
      const current = db.metadataJobs[id];
      if (current && (current.state === "queued" || current.state === "running"))
        return HttpResponse.json(current, { status: 202 });
      jobs += 1;
      const job: Schemas["Job"] = {
        id: `01920000-0000-7000-8000-0000000d${(0x100 + jobs).toString(16).padStart(4, "0")}`,
        kind: "metadata.fetch",
        state: "queued",
        attempts: 0,
        max_attempts: 5,
        created_at: new Date().toISOString(),
      };
      db.metadataJobs[id] = job;
      return HttpResponse.json(job, { status: 202 });
    }),

    http.post("*/v1/admin/packages/:id/metadata/apply", async ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const id = String(params.id);
      const pkg = find(id);
      if (!pkg) return problem(404, "not_found", "Not found");
      const ifMatch = request.headers.get("If-Match");
      if (!ifMatch) return problem(428, "precondition_required", "If-Match is required");
      if (ifMatch !== etagOf(db, id))
        return problem(412, "precondition_failed", "The package changed since you loaded it");
      const body = (await request.json()) as Schemas["MetadataApply"];
      const candidate = (db.candidates[id] ?? []).find(
        (c) => c.source === body.source && c.external_id === body.external_id,
      );
      if (!candidate || body.fields.length === 0)
        return problem(400, "validation_failed", "Invalid request", {
          errors: [{ field: "external_id", code: "unknown_candidate" }],
        });
      const next = { ...pkg, field_sources: { ...pkg.field_sources } };
      const d = candidate.data;
      for (const field of body.fields) {
        if (!body.overwrite_admin_fields && pkg.field_sources[field] === "admin") continue;
        switch (field) {
          case "title":
          case "summary":
          case "description":
          case "release_date":
          case "developer":
          case "publisher":
            if (d[field] !== undefined) next[field] = d[field];
            break;
          case "genres":
            if (d.genres) next.genres = d.genres;
            break;
          case "external_ids":
            if (d.external?.steam_app_id) next.steam_app_id = d.external.steam_app_id;
            if (d.external?.igdb_id) next.igdb_id = d.external.igdb_id;
            break;
          default:
            // Images: fetched by a job later.
            break;
        }
        next.field_sources[field] = body.source;
      }
      next.updated_at = new Date().toISOString();
      db.packages = db.packages.map((p) => (p.id === id ? next : p));
      db.etags[id] = (db.etags[id] ?? 1) + 1;
      return HttpResponse.json(next, { headers: { ETag: etagOf(db, id) } });
    }),

    http.post("*/v1/admin/packages/:id/assets", async ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const id = String(params.id);
      const pkg = find(id);
      if (!pkg) return problem(404, "not_found", "Not found");
      const form = await request.formData();
      const kind = String(form.get("kind")) as Schemas["Asset"]["kind"];
      const file = form.get("file");
      if (!(file instanceof Blob)) return problem(400, "validation_failed", "Invalid request");
      if (file.size > 10 * 1024 * 1024)
        return problem(413, "payload_too_large", "Image larger than 10 MiB");
      const type = file.type || "application/octet-stream";
      if (!["image/jpeg", "image/png", "image/webp"].includes(type))
        return problem(415, "unsupported_media_type", "Only JPEG, PNG and WebP");
      assets += 1;
      const asset: Schemas["Asset"] = {
        id: assetId(assets),
        kind,
        url: `/v1/assets/${assetId(assets)}`,
        width: 1280,
        height: 720,
        content_type: type as Schemas["Asset"]["content_type"],
        source: "upload",
      };
      if (kind === "screenshot") {
        db.packages = db.packages.map((p) =>
          p.id === id ? { ...p, screenshots: [...(p.screenshots ?? []), asset] } : p,
        );
      }
      db.uploadedAssets = { ...(db.uploadedAssets ?? {}), [asset.id]: asset };
      return HttpResponse.json(asset, { status: 201 });
    }),

    http.delete("*/v1/admin/assets/:assetId", ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const target = String(params.assetId);
      let found = false;
      db.packages = db.packages.map((p) => {
        const next = { ...p };
        for (const slot of ["cover", "hero", "logo"] as const) {
          if (next[slot]?.id === target) {
            delete next[slot];
            found = true;
          }
        }
        if (next.screenshots?.some((s) => s.id === target)) {
          next.screenshots = next.screenshots.filter((s) => s.id !== target);
          found = true;
        }
        return next;
      });
      if (!found) return problem(404, "not_found", "Not found");
      return new HttpResponse(null, { status: 204 });
    }),

    http.get("*/v1/assets/:assetId", ({ params }) => {
      if (db.brokenAssets.includes(String(params.assetId)))
        return problem(404, "not_found", "Not found");
      return new HttpResponse(PNG_1X1, { headers: { "Content-Type": "image/png" } });
    }),
  ];
}
