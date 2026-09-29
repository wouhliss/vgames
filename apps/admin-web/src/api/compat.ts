// Compatibility profiles (09-compatibility §4): the latest signed profile per target, and publishing
// a new revision (document + signature made in the upload workers).
import type { Schemas } from "@vgames/api-client";
import { call, client } from "./http";
import { CompatProfilesSchema, ServerIdSchema, SignedCompatProfileSchema } from "./schemas";

export const compatKeys = {
  profiles: (packageId: string) => ["compat-profiles", packageId] as const,
  server: ["server-info"] as const,
};

export function serverInfo(signal?: AbortSignal) {
  return call(ServerIdSchema, (o) => client.GET("/.well-known/vgames.json", o), signal);
}

export function compatProfiles(packageId: string, signal?: AbortSignal) {
  return call(
    CompatProfilesSchema,
    (o) =>
      client.GET("/v1/packages/{package_id}/compat", {
        ...o,
        params: { path: { package_id: packageId } },
      }),
    signal,
  );
}

export function putCompatProfile(
  packageId: string,
  target: "linux" | "macos",
  body: { document: string; signature: Schemas["SignatureEnvelope"] },
) {
  return call(SignedCompatProfileSchema, (o) =>
    client.PUT("/v1/admin/packages/{package_id}/compat/{target}", {
      ...o,
      params: { path: { package_id: packageId, target } },
      body,
    }),
  );
}
