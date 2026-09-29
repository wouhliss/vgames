// MSW handlers for server administration (users, allowlist, settings, trust, jobs, audit log), with
// the API's rules: owners only for role changes, settings and trust bundles; no disabling yourself;
// at least one active owner; admins can't disable admins or owners.
import type { Schemas } from "@vgames/api-client";
import { HttpResponse, http } from "msw";
import { currentUser, type MockDb } from "./db";
import { guard, problem } from "./handlers";

const page = <T>(all: T[], url: URL) => {
  const limit = Math.min(200, Math.max(1, Number(url.searchParams.get("limit") ?? 50)));
  const offset = Number((url.searchParams.get("cursor") ?? "o0").slice(1)) || 0;
  const items = all.slice(offset, offset + limit);
  return offset + limit < all.length ? { items, next_cursor: `o${offset + limit}` } : { items };
};

export function serverHandlers(db: MockDb) {
  const rank = { user: 0, admin: 1, owner: 2 } as const;
  const activeOwners = () => db.users.filter((u) => u.role === "owner" && !u.disabled_at);
  const settingsEtag = () => `W/"s${db.settingsEtag}"`;
  return [
    http.get("*/v1/admin/users", ({ request }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const url = new URL(request.url);
      const q = (url.searchParams.get("q") ?? "").toLowerCase();
      const role = url.searchParams.get("role");
      const all = db.users
        .filter((u) => !role || u.role === role)
        .filter(
          (u) => !q || u.username.toLowerCase().includes(q) || (u.discord_id ?? "").includes(q),
        );
      return HttpResponse.json(page(all, url));
    }),

    http.patch("*/v1/admin/users/:id", async ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const caller = currentUser(db);
      const target = db.users.find((u) => u.id === params.id);
      if (!caller) return problem(401, "unauthenticated", "Sign in required");
      if (!target) return problem(404, "not_found", "Not found");
      const patch = (await request.json()) as Schemas["AdminUserPatch"];
      if (patch.role && caller.role !== "owner")
        return problem(403, "forbidden", "Not allowed", {
          detail: "Only an owner can change roles.",
        });
      const isActiveOwner = target.role === "owner" && !target.disabled_at;
      if (patch.disabled !== undefined) {
        if (patch.disabled && target.id === caller.id)
          return problem(403, "cannot_disable_self", "You cannot disable your own account");
        if (caller.role === "admin" && rank[target.role] >= rank.admin)
          return problem(403, "forbidden", "Not allowed", {
            detail: "Admins cannot disable or enable other admins or owners.",
          });
        if (patch.disabled && isActiveOwner && activeOwners().length <= 1)
          return problem(409, "last_owner", "The server must keep at least one active owner");
      }
      if (patch.role && isActiveOwner && patch.role !== "owner" && activeOwners().length <= 1)
        return problem(409, "last_owner", "The server must keep at least one active owner");
      if (patch.disabled_reason && [...patch.disabled_reason].length > 500)
        return problem(400, "validation_failed", "Invalid", {
          errors: [
            { field: "disabled_reason", code: "max_length", message: "at most 500 characters" },
          ],
        });
      const next = { ...target };
      if (patch.role) next.role = patch.role;
      if (patch.disabled === true) {
        next.disabled_at = new Date().toISOString();
        if (patch.disabled_reason) next.disabled_reason = patch.disabled_reason;
      }
      if (patch.disabled === false) {
        delete next.disabled_at;
        delete next.disabled_reason;
      }
      db.users = db.users.map((u) => (u.id === target.id ? next : u));
      return HttpResponse.json(next);
    }),

    http.get("*/v1/admin/allowlist", ({ request }) => {
      const denied = guard(db, request);
      return denied ?? HttpResponse.json({ items: db.allowlist });
    }),

    http.post("*/v1/admin/allowlist", async ({ request }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const body = (await request.json()) as { discord_id?: string; note?: string };
      if (!/^[0-9]{5,25}$/.test(body.discord_id ?? ""))
        return problem(400, "validation_failed", "Invalid", {
          errors: [{ field: "discord_id", code: "pattern", message: "must be 5-25 digits" }],
        });
      if (db.allowlist.some((e) => e.discord_id === body.discord_id))
        return problem(
          409,
          "already_allowlisted",
          "This Discord account is already on the allowlist",
        );
      const caller = currentUser(db);
      const entry: Schemas["AllowlistEntry"] = {
        discord_id: body.discord_id ?? "",
        ...(body.note ? { note: body.note } : {}),
        ...(caller ? { added_by: { id: caller.id, username: caller.username } } : {}),
        created_at: new Date().toISOString(),
      };
      db.allowlist = [entry, ...db.allowlist];
      return HttpResponse.json(entry, { status: 201 });
    }),

    http.delete("*/v1/admin/allowlist/:discordId", ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      if (!db.allowlist.some((e) => e.discord_id === params.discordId))
        return problem(404, "not_found", "Not found");
      db.allowlist = db.allowlist.filter((e) => e.discord_id !== params.discordId);
      return new HttpResponse(null, { status: 204 });
    }),

    http.get("*/v1/admin/settings", ({ request }) => {
      const denied = guard(db, request);
      return denied ?? HttpResponse.json(db.settings, { headers: { ETag: settingsEtag() } });
    }),

    http.patch("*/v1/admin/settings", async ({ request }) => {
      const denied = guard(db, request, "owner");
      if (denied) return denied;
      const ifMatch = request.headers.get("If-Match");
      if (!ifMatch) return problem(428, "precondition_required", "If-Match is required");
      if (ifMatch !== settingsEtag())
        return problem(412, "precondition_failed", "Changed since you loaded it");
      const patch = (await request.json()) as Schemas["ServerSettingsPatch"];
      const errors: { field: string; code: string; message: string }[] = [];
      if (
        patch.name !== undefined &&
        ([...patch.name.trim()].length < 1 || [...patch.name].length > 100)
      )
        errors.push({ field: "name", code: "length", message: "must be 1-100 characters" });
      if (patch.motd !== undefined && [...patch.motd].length > 500)
        errors.push({ field: "motd", code: "max_length", message: "at most 500 characters" });
      if (errors.length) return problem(400, "validation_failed", "Invalid", { errors });
      db.settings = { ...db.settings, ...patch };
      db.settingsEtag += 1;
      return HttpResponse.json(db.settings, { headers: { ETag: settingsEtag() } });
    }),

    http.get("*/v1/admin/trust/publisher-keys", ({ request }) => {
      const denied = guard(db, request);
      return (
        denied ??
        HttpResponse.json({
          bundle_version: db.trust.version,
          bundle_expires_at: db.trust.expiresAt,
          items: db.trust.keys,
        })
      );
    }),

    http.post("*/v1/admin/trust/bundles", async ({ request }) => {
      const denied = guard(db, request, "owner");
      if (denied) return denied;
      const body = (await request.json()) as { bundle?: string; signature?: string };
      let doc: { format?: string; version?: number; server_id?: string };
      try {
        doc = JSON.parse(atob(body.bundle ?? ""));
      } catch {
        return problem(422, "invalid_bundle", "The trust bundle is invalid", {
          detail: "not JSON",
        });
      }
      if (doc.format !== "vgames.trust/1")
        return problem(422, "invalid_bundle", "The trust bundle is invalid");
      if (atob(body.signature ?? "") !== "root-signature")
        return problem(422, "bad_signature", "The bundle is not signed by this server's root key");
      if ((doc.version ?? 0) <= db.trust.version)
        return problem(409, "stale_version", "A newer trust bundle is already stored");
      db.trust = { ...db.trust, version: doc.version ?? 0 };
      return HttpResponse.json({ version: db.trust.version }, { status: 201 });
    }),

    http.get("*/v1/admin/jobs", ({ request }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const url = new URL(request.url);
      const state = url.searchParams.get("state");
      const kind = url.searchParams.get("kind");
      const all = db.jobs.filter(
        (j) => (!state || j.state === state) && (!kind || j.kind === kind),
      );
      return HttpResponse.json(page(all, url));
    }),

    http.post("*/v1/admin/jobs/:id/retry", ({ request, params }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const job = db.jobs.find((j) => j.id === params.id);
      if (!job) return problem(404, "not_found", "Not found");
      if (job.state !== "failed" && job.state !== "dead")
        return problem(409, "job_already_queued", "This job is already queued or running");
      const next = { ...job, state: "queued" as const, attempts: 0 };
      db.jobs = db.jobs.map((j) => (j.id === job.id ? next : j));
      return HttpResponse.json(next);
    }),

    http.get("*/v1/admin/audit-log", ({ request }) => {
      const denied = guard(db, request);
      if (denied) return denied;
      const url = new URL(request.url);
      const p = url.searchParams;
      const since = p.get("since");
      const until = p.get("until");
      if ((since && Number.isNaN(Date.parse(since))) || (until && Number.isNaN(Date.parse(until))))
        return problem(400, "validation_failed", "Invalid", {
          errors: [{ field: "since", code: "format" }],
        });
      const all = db.audit.filter(
        (e) =>
          (!p.get("actor_user_id") || e.actor?.id === p.get("actor_user_id")) &&
          (!p.get("action") || e.action === p.get("action")) &&
          (!p.get("target_type") || e.target_type === p.get("target_type")) &&
          (!p.get("target_id") || e.target_id === p.get("target_id")) &&
          (!since || e.created_at >= since) &&
          (!until || e.created_at <= until),
      );
      return HttpResponse.json(page(all, url));
    }),
  ];
}
