// Server administration endpoints (users, allowlist, settings, trust, jobs, audit log).
import type { Schemas } from "@vgames/api-client";
import { call, callEmpty, client } from "./http";
import {
  AdminUserPageSchema,
  AdminUserSchema,
  AllowlistEntrySchema,
  AllowlistSchema,
  AuditPageSchema,
  BundleStoredSchema,
  JobPageSchema,
  JobSchema,
  PublisherKeysSchema,
  ServerSettingsSchema,
} from "./schemas";

export const serverKeys = {
  users: (f: object) => ["admin-users", f] as const,
  allowlist: ["admin-allowlist"] as const,
  settings: ["admin-settings"] as const,
  trust: ["admin-trust"] as const,
  jobs: (f: object) => ["admin-jobs", f] as const,
  audit: (f: object) => ["admin-audit", f] as const,
};

const strip = <T extends Record<string, unknown>>(o: T) =>
  Object.fromEntries(
    Object.entries(o).filter(([, v]) => v !== undefined && v !== ""),
  ) as Partial<T>;

export function listUsers(
  f: { q: string; role: string },
  cursor: string | null,
  signal?: AbortSignal,
) {
  const query = strip({ limit: 50, q: f.q.trim(), role: f.role, cursor: cursor ?? undefined });
  return call(
    AdminUserPageSchema,
    (o) =>
      client.GET("/v1/admin/users", {
        ...o,
        params: { query: query as never },
      }),
    signal,
  );
}

export function updateUser(id: string, patch: Schemas["AdminUserPatch"]) {
  return call(AdminUserSchema, (o) =>
    client.PATCH("/v1/admin/users/{user_id}", {
      ...o,
      params: { path: { user_id: id } },
      body: patch,
      headers: { "Content-Type": "application/merge-patch+json" },
    }),
  );
}

export function listAllowlist(signal?: AbortSignal) {
  return call(AllowlistSchema, (o) => client.GET("/v1/admin/allowlist", o), signal);
}

export function addAllowlist(discordId: string, note: string) {
  return call(AllowlistEntrySchema, (o) =>
    client.POST("/v1/admin/allowlist", {
      ...o,
      body: { discord_id: discordId, ...(note.trim() ? { note: note.trim() } : {}) },
    }),
  );
}

export function removeAllowlist(discordId: string) {
  return callEmpty((o) =>
    client.DELETE("/v1/admin/allowlist/{discord_id}", {
      ...o,
      params: { path: { discord_id: discordId } },
    }),
  );
}

export function getSettings(signal?: AbortSignal) {
  return call(ServerSettingsSchema, (o) => client.GET("/v1/admin/settings", o), signal);
}

export function updateSettings(etag: string, patch: Schemas["ServerSettingsPatch"]) {
  return call(ServerSettingsSchema, (o) =>
    client.PATCH("/v1/admin/settings", {
      ...o,
      params: { header: { "If-Match": etag } },
      body: patch,
      headers: { "Content-Type": "application/merge-patch+json" },
    }),
  );
}

export function listPublisherKeys(signal?: AbortSignal) {
  return call(PublisherKeysSchema, (o) => client.GET("/v1/admin/trust/publisher-keys", o), signal);
}

export function uploadBundle(body: Schemas["SignedTrustBundle"]) {
  return call(BundleStoredSchema, (o) => client.POST("/v1/admin/trust/bundles", { ...o, body }));
}

export function listJobs(
  f: { state: string; kind: string },
  cursor: string | null,
  signal?: AbortSignal,
) {
  const query = strip({
    limit: 50,
    state: f.state,
    kind: f.kind.trim(),
    cursor: cursor ?? undefined,
  });
  return call(
    JobPageSchema,
    (o) => client.GET("/v1/admin/jobs", { ...o, params: { query: query as never } }),
    signal,
  );
}

export function retryJob(id: string) {
  return call(JobSchema, (o) =>
    client.POST("/v1/admin/jobs/{job_id}/retry", { ...o, params: { path: { job_id: id } } }),
  );
}

export interface AuditFilters {
  actor: string;
  action: string;
  targetType: string;
  targetId: string;
  since: string;
  until: string;
}

export function listAudit(f: AuditFilters, cursor: string | null, signal?: AbortSignal) {
  const query = strip({
    limit: 50,
    actor_user_id: f.actor.trim(),
    action: f.action.trim(),
    target_type: f.targetType.trim(),
    target_id: f.targetId.trim(),
    since: f.since,
    until: f.until,
    cursor: cursor ?? undefined,
  });
  return call(
    AuditPageSchema,
    (o) => client.GET("/v1/admin/audit-log", { ...o, params: { query: query as never } }),
    signal,
  );
}
