// The admin web's only door to the API.
//
// - Same-origin cookie auth; unsafe methods carry `X-CSRF-Token` from the `__Host-vgames_csrf`
//   cookie (01-security §4.2).
// - Every request times out after 30 s (AbortController), and callers can cancel.
// - Error bodies are parsed as RFC 9457 problem details into a typed ApiError.
// - Every successful body is validated with zod, even though the client is generated: the server
//   may be a different version than this UI.
import { createApiClient } from "@vgames/api-client";
import type { z } from "zod";
import { ApiError, type ApiProblem, ProblemSchema } from "./errors";

export const REQUEST_TIMEOUT_MS = 30_000;
const CSRF_COOKIE = "__Host-vgames_csrf";
const UNSAFE = new Set(["POST", "PUT", "PATCH", "DELETE"]);

let csrfFallback: string | null = null;

/** Set from `GET /v1/me` (`csrf_token`) in case the cookie is not readable. */
export function setCsrfFallback(token: string | null): void {
  csrfFallback = token;
}

export function readCsrfToken(): string | null {
  for (const part of document.cookie.split(";")) {
    const [name, ...rest] = part.trim().split("=");
    if (name === CSRF_COOKIE) return decodeURIComponent(rest.join("="));
  }
  return csrfFallback;
}

class TimeoutSignal extends Error {}

/** fetch with CSRF, same-origin credentials and the time limit. Throws ApiError for no-response cases. */
export async function policyFetch(
  input: Request,
  timeoutMs = REQUEST_TIMEOUT_MS,
): Promise<Response> {
  const headers = new Headers(input.headers);
  if (UNSAFE.has(input.method.toUpperCase())) {
    const token = readCsrfToken();
    if (token) headers.set("X-CSRF-Token", token);
  }
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(new TimeoutSignal()), timeoutMs);
  const onCallerAbort = () => controller.abort(input.signal.reason);
  if (input.signal.aborted) onCallerAbort();
  else input.signal.addEventListener("abort", onCallerAbort, { once: true });
  const request = new Request(input, {
    headers,
    signal: controller.signal,
    credentials: "same-origin",
  });
  try {
    return await fetch(request);
  } catch {
    if (controller.signal.reason instanceof TimeoutSignal)
      throw new ApiError({ kind: "timeout", afterMs: timeoutMs });
    if (input.signal.aborted) throw new ApiError({ kind: "aborted" });
    throw new ApiError({ kind: "network" });
  } finally {
    clearTimeout(timer);
    input.signal.removeEventListener("abort", onCallerAbort);
  }
}

export const client = createApiClient({
  baseUrl: typeof window === "undefined" ? "" : window.location.origin,
  fetch: (request: Request) => policyFetch(request),
});

function retryAfterSeconds(response: Response): number | null {
  const raw = response.headers.get("Retry-After");
  if (!raw) return null;
  const seconds = Number(raw);
  if (Number.isFinite(seconds) && seconds >= 0) return Math.min(seconds, 3600);
  const date = Date.parse(raw);
  return Number.isNaN(date)
    ? null
    : Math.max(0, Math.min(3600, Math.ceil((date - Date.now()) / 1000)));
}

function toProblem(body: unknown, response: Response): ApiProblem {
  let parsed = body;
  if (typeof body === "string") {
    try {
      parsed = JSON.parse(body);
    } catch {
      parsed = null;
    }
  }
  const result = ProblemSchema.safeParse(parsed);
  if (result.success) return result.data;
  // Proxies and crashes answer with HTML or nothing: keep the status, invent a stable code.
  return {
    type: "about:blank",
    title: response.statusText || `HTTP ${response.status}`,
    status: response.status >= 400 && response.status <= 599 ? response.status : 500,
    code: `http_${response.status}`,
  };
}

export interface ApiResponse<T> {
  data: T;
  /** Value for `If-Match` on the next write, when the resource is versioned. */
  etag: string | null;
  status: number;
}

type RawCall = (opts: { parseAs: "text"; signal?: AbortSignal }) => Promise<{
  data?: unknown;
  error?: unknown;
  response: Response;
}>;

async function send(run: RawCall, signal: AbortSignal | undefined) {
  try {
    return await run(signal ? { parseAs: "text", signal } : { parseAs: "text" });
  } catch (e) {
    if (e instanceof ApiError) throw e;
    if (signal?.aborted) throw new ApiError({ kind: "aborted" });
    throw new ApiError({ kind: "network" });
  }
}

let onUnauthorized: (() => void) | null = null;

/**
 * Called on every 401, whoever made the request (queries, mutations and direct calls from forms):
 * the app keeps unsaved input and goes to sign-in.
 */
export function setUnauthorizedHandler(handler: (() => void) | null): void {
  onUnauthorized = handler;
}

/** The typed error for a non-2xx response whose body was `error` (text or parsed). */
export function failure(response: Response, error: unknown): ApiError {
  if (response.status === 401) onUnauthorized?.();
  return new ApiError({
    kind: "http",
    status: response.status,
    problem: toProblem(error, response),
    retryAfterSeconds: retryAfterSeconds(response),
    requestId: response.headers.get("X-Request-Id"),
  });
}

/**
 * Runs a generated-client call and validates the JSON body.
 *
 *     const me = await call(MeSchema, (o) => client.GET("/v1/me", o));
 */
export async function call<S extends z.ZodType>(
  schema: S,
  run: RawCall,
  signal?: AbortSignal,
): Promise<ApiResponse<z.infer<S>>> {
  const { data, error, response } = await send(run, signal);
  if (!response.ok) throw failure(response, error);
  const requestId = response.headers.get("X-Request-Id");
  let json: unknown;
  try {
    json = JSON.parse(typeof data === "string" ? data : "");
  } catch {
    throw new ApiError({ kind: "malformed", status: response.status, requestId });
  }
  const result = schema.safeParse(json);
  if (!result.success) {
    throw new ApiError({
      kind: "schema",
      status: response.status,
      requestId,
      issues: result.error.issues.map((i) => `${i.path.join(".") || "(root)"}: ${i.message}`),
    });
  }
  return { data: result.data, etag: response.headers.get("ETag"), status: response.status };
}

/** For endpoints that answer 204 No Content. */
export async function callEmpty(run: RawCall, signal?: AbortSignal): Promise<{ status: number }> {
  const { error, response } = await send(run, signal);
  if (!response.ok) throw failure(response, error);
  return { status: response.status };
}

/** A fresh `Idempotency-Key` (16–128 chars `[A-Za-z0-9_-]`, 03-api §1). */
export function newIdempotencyKey(): string {
  return crypto.randomUUID().replaceAll("-", "");
}
