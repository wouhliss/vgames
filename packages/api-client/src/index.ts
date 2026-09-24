// Typed client for the vgames /v1 API, generated from openapi/openapi.yaml.
// Used by apps/admin-web only: the desktop UI never calls the API directly
// (all desktop networking lives in the Rust core). Owner: Agent 3.
//
// Run `pnpm api:types` after every change to openapi/openapi.yaml.
import createClient, { type ClientOptions } from "openapi-fetch";
import type { components, paths } from "./schema";

export type { components, paths };
export type Schemas = components["schemas"];

/**
 * A typed client. Bodies are returned as text (`parseAs: "text"`): callers parse and validate them
 * (admin-web does this with zod), so malformed JSON and schema drift are handled in one place.
 */
export function createApiClient(options: ClientOptions) {
  return createClient<paths>(options);
}

export type ApiClient = ReturnType<typeof createApiClient>;
