// MSW handlers for versions (02-package-format §6) plus an emulated GCS for resumable pack uploads
// and the manifest PUT. Finalize answers the same 422 codes as apps/api/src/finalize.rs.
import type { Schemas } from "@vgames/api-client";
import { HttpResponse, http } from "msw";
import { IDS, type MockDb, SERVER_ID, versionId } from "./db";
import { guard, problem } from "./handlers";

export const GCS = "https://storage.mock.test";

export function versionHandlers(db: MockDb) {
  let seq = 0x100;
  const idempotency: Record<string, string> = {};
  const all = () => Object.values(db.versions).flat();
  const find = (id: string) => all().find((v) => v.id === id);
  const update = (id: string, patch: Partial<Schemas["Version"]>) => {
    for (const [pkg, list] of Object.entries(db.versions)) {
      db.versions[pkg] = list.map((v) => (v.id === id ? { ...v, ...patch } : v));
    }
    return find(id);
  };

  /** Each status read moves verification on by a quarter. */
  function advance(id: string) {
    const v = find(id);
    if (v?.state !== "verifying") return v;
    const progress = Math.min(1, (v.verify_progress ?? 0) + 0.25);
    if (progress < 1) return update(id, { verify_progress: progress });
    if ((db.verifyOutcome[id] ?? "ok") === "fail")
      return update(id, {
        state: "failed",
        verify_progress: 1,
        failure_reason: "pack 2: chunk 17 hash mismatch",
      });
    return update(id, {
      state: "ready",
      verify_progress: 1,
      verified_at: new Date().toISOString(),
    });
  }

  return [
    http.get("*/v1/admin/packages/:id/versions", ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const id = String(params.id);
      if (!db.packages.some((p) => p.id === id)) return problem(404, "not_found", "Not found");
      const items = (db.versions[id] ?? []).map((v) =>
        v.state === "verifying" ? (advance(v.id) ?? v) : v,
      );
      return HttpResponse.json({ items });
    }),

    http.post("*/v1/admin/packages/:id/versions", async ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const id = String(params.id);
      if (!db.packages.some((p) => p.id === id)) return problem(404, "not_found", "Not found");
      const key = request.headers.get("Idempotency-Key") ?? "";
      const known = key ? idempotency[key] : undefined;
      const existing = known ? find(known) : undefined;
      if (existing) return HttpResponse.json(existing, { status: 201 });
      const body = (await request.json()) as Schemas["VersionCreate"];
      const label = body.version_label?.trim() ?? "";
      if ([...label].length < 1 || [...label].length > 64)
        return problem(400, "validation_failed", "Invalid request", {
          errors: [{ field: "version_label", code: "length", message: "must be 1-64 characters" }],
        });
      seq += 1;
      const list = db.versions[id] ?? [];
      const version: Schemas["Version"] = {
        id: versionId(seq),
        package_id: id,
        server_id: SERVER_ID,
        platform: body.platform,
        sequence: list.reduce((n, v) => Math.max(n, v.sequence), 0) + 1,
        version_label: label,
        state: "uploading",
        created_at: new Date().toISOString(),
        created_by: { id: IDS.admin, username: "adrian", display_name: "Adrian" },
      };
      db.versions[id] = [version, ...list];
      if (key) idempotency[key] = version.id;
      return HttpResponse.json(version, { status: 201 });
    }),

    http.get("*/v1/admin/versions/:vid", ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const v = advance(String(params.vid));
      if (!v) return problem(404, "not_found", "Not found");
      return HttpResponse.json(v);
    }),

    http.delete("*/v1/admin/versions/:vid", ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const v = find(String(params.vid));
      if (!v) return problem(404, "not_found", "Not found");
      if (v.state === "published" || v.state === "yanked" || v.state === "aborted")
        return problem(409, "version_not_abortable", "Only unpublished versions can be aborted");
      update(v.id, { state: "aborted" });
      return new HttpResponse(null, { status: 204 });
    }),

    http.post("*/v1/admin/versions/:vid/packs/:pack/upload-session", ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const v = find(String(params.vid));
      if (!v) return problem(404, "not_found", "Not found");
      if (v.state !== "uploading") return problem(409, "version_not_uploading", "Not uploading");
      const expired = db.faults.expiredStarts > 0;
      if (expired) db.faults.expiredStarts -= 1;
      const target: Schemas["UploadTarget"] = {
        url: `${GCS}/start/${v.id}/${params.pack}${expired ? "?expired=1" : ""}`,
        method: "POST",
        headers: { "x-goog-resumable": "start", "Content-Type": "application/octet-stream" },
        expires_at: new Date(Date.now() + (expired ? -1000 : 15 * 60_000)).toISOString(),
      };
      return HttpResponse.json(target);
    }),

    // GCS: start a resumable session (the Location header is the session URI).
    http.post(`${GCS}/start/:vid/:pack`, ({ request, params }) => {
      if (new URL(request.url).searchParams.has("expired"))
        return new HttpResponse("<Error><Code>ExpiredToken</Code></Error>", { status: 400 });
      if (request.headers.get("x-goog-resumable") !== "start")
        return new HttpResponse("missing x-goog-resumable", { status: 400 });
      const key = `${params.vid}/${params.pack}`;
      db.gcs[key] ??= { received: 0, total: null, complete: false };
      return new HttpResponse(null, {
        status: 201,
        headers: { Location: `${GCS}/session/${params.vid}/${params.pack}` },
      });
    }),

    // GCS: upload a piece, or ask for the offset (`bytes */total`).
    http.put(`${GCS}/session/:vid/:pack`, async ({ request, params }) => {
      if (db.faults.networkDown) return HttpResponse.error();
      if (db.faults.gcsErrors > 0) {
        db.faults.gcsErrors -= 1;
        return new HttpResponse("backend error", { status: 503 });
      }
      const key = `${params.vid}/${params.pack}`;
      const state = db.gcs[key];
      if (!state) return new HttpResponse("no such session", { status: 404 });
      const range = request.headers.get("Content-Range") ?? "";
      const body = await request.arrayBuffer();
      const query = /^bytes \*\/(\d+|\*)$/.exec(range);
      const piece = /^bytes (\d+)-(\d+)\/(\d+|\*)$/.exec(range);
      const reply = () =>
        state.complete
          ? new HttpResponse(null, { status: 200 })
          : new HttpResponse(null, {
              status: 308,
              headers: state.received > 0 ? { Range: `bytes=0-${state.received - 1}` } : {},
            });
      if (query) {
        if (query[1] !== "*") state.total = Number(query[1]);
        if (state.total !== null && state.received === state.total) state.complete = true;
        return reply();
      }
      if (!piece) return new HttpResponse("bad Content-Range", { status: 400 });
      const start = Number(piece[1]);
      const end = Number(piece[2]);
      if (start !== state.received || end - start + 1 !== body.byteLength) return reply(); // out of order: tell the client where we are
      state.received = end + 1;
      if (db.faults.downAfterPieces !== undefined) {
        db.faults.downAfterPieces -= 1;
        if (db.faults.downAfterPieces <= 0) {
          db.faults.networkDown = true;
          delete db.faults.downAfterPieces;
        }
      }
      if (piece[3] !== "*") state.total = Number(piece[3]);
      if (state.total !== null && state.received === state.total) state.complete = true;
      return reply();
    }),

    http.post("*/v1/admin/versions/:vid/manifest-upload", ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const v = find(String(params.vid));
      if (!v) return problem(404, "not_found", "Not found");
      if (v.state !== "uploading") return problem(409, "version_not_uploading", "Not uploading");
      const target: Schemas["UploadTarget"] = {
        url: `${GCS}/manifest/${v.id}`,
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        expires_at: new Date(Date.now() + 15 * 60_000).toISOString(),
      };
      return HttpResponse.json(target);
    }),

    http.put(`${GCS}/manifest/:vid`, async ({ request, params }) => {
      const body = await request.arrayBuffer();
      db.manifests[String(params.vid)] = { size: body.byteLength };
      return new HttpResponse(null, { status: 200 });
    }),

    http.post("*/v1/admin/versions/:vid/finalize", async ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const v = find(String(params.vid));
      if (!v) return problem(404, "not_found", "Not found");
      if (v.state !== "uploading")
        return problem(
          409,
          "version_not_uploading",
          "This version is already finalized or aborted",
        );
      const body = (await request.json()) as Schemas["FinalizeRequest"];
      const unprocessable = (code: string, title: string, extra = {}) =>
        problem(422, code, title, extra);
      const trusted = db.trustedKeys[body.signature.key_id];
      if (!trusted) return unprocessable("publisher_key_untrusted", "The key is not trusted");
      if (trusted !== IDS.admin && trusted !== IDS.owner)
        return unprocessable("publisher_key_not_yours", "The key belongs to someone else");
      if (body.signature.payload_blake3 !== body.manifest_blake3)
        return unprocessable(
          "manifest_hash_mismatch",
          "The signature is over different manifest bytes",
        );
      const manifest = db.manifests[v.id];
      if (!manifest) return unprocessable("manifest_missing", "The manifest has not been uploaded");
      if (manifest.size !== body.manifest_size)
        return unprocessable("manifest_hash_mismatch", "The uploaded manifest does not match");
      const packs = Object.entries(db.gcs).filter(([k]) => k.startsWith(`${v.id}/`));
      const missing = packs.filter(([, s]) => !s.complete).map(([k]) => k.split("/")[1]);
      if (packs.length === 0 || missing.length > 0)
        return unprocessable("pack_missing", "Some packs have not been uploaded", {
          errors: missing.map((i) => ({ field: `packs[${i}]`, code: "missing" })),
        });
      const total = packs.reduce((n, [, s]) => n + (s.total ?? 0), 0);
      const updated = update(v.id, {
        state: "verifying",
        verify_progress: 0,
        pack_count: packs.length,
        total_size: total,
        publisher_key_id: body.signature.key_id,
        finalized_at: new Date().toISOString(),
      });
      return HttpResponse.json(updated, { status: 202 });
    }),

    http.post("*/v1/admin/versions/:vid/publish", ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const v = find(String(params.vid));
      if (!v) return problem(404, "not_found", "Not found");
      if (v.state !== "ready")
        return problem(409, "version_not_ready", "Only verified versions can be published");
      for (const other of db.versions[v.package_id] ?? []) {
        if (other.platform === v.platform && other.is_current_release)
          update(other.id, { is_current_release: false });
      }
      return HttpResponse.json(
        update(v.id, {
          state: "published",
          is_current_release: true,
          published_at: new Date().toISOString(),
        }),
      );
    }),

    http.post("*/v1/admin/versions/:vid/yank", async ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const v = find(String(params.vid));
      if (!v) return problem(404, "not_found", "Not found");
      const body = (await request.json()) as { reason?: string };
      const n = [...(body.reason ?? "").trim()].length;
      if (n < 3 || n > 500)
        return problem(400, "validation_failed", "Invalid request", {
          errors: [{ field: "reason", code: "length", message: "must be 3-500 characters" }],
        });
      if (v.state !== "published")
        return problem(409, "version_not_published", "Only published versions can be yanked");
      return HttpResponse.json(
        update(v.id, {
          state: "yanked",
          is_current_release: false,
          yanked_at: new Date().toISOString(),
        }),
      );
    }),
  ];
}
