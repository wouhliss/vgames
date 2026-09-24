import { delay, HttpResponse, http } from "msw";
import { describe, expect, it } from "vitest";
import { server } from "../test/setup";
import { ApiError } from "./errors";
import { call, callEmpty, client, newIdempotencyKey, policyFetch } from "./http";
import { retryDelay, shouldRetry } from "./query";
import { MeSchema } from "./schemas";

const ME = {
  user: {
    id: "01920000-0000-7000-8000-00000000a002",
    username: "adrian",
    role: "admin",
    created_at: "2026-09-24T10:00:00Z",
  },
  session: {
    id: "01920000-0000-7000-8000-00000000c001",
    kind: "web",
    created_at: "2026-09-24T10:00:00Z",
    last_used_at: "2026-09-24T10:00:00Z",
    current: true,
  },
};

const getMe = () => call(MeSchema, (o) => client.GET("/v1/me", o));

function problem(
  status: number,
  code: string,
  extra: Record<string, unknown> = {},
  headers: Record<string, string> = {},
) {
  return HttpResponse.json(
    { type: `urn:vgames:problem:${code}`, title: code, status, code, ...extra },
    { status, headers: { "Content-Type": "application/problem+json", ...headers } },
  );
}

async function failure(promise: Promise<unknown>): Promise<ApiError> {
  try {
    await promise;
  } catch (e) {
    if (e instanceof ApiError) return e;
    throw e;
  }
  throw new Error("expected the call to fail");
}

describe("successful responses", () => {
  it("validates the body and returns the ETag", async () => {
    server.use(http.get("*/v1/me", () => HttpResponse.json(ME, { headers: { ETag: '"v3"' } })));
    const res = await getMe();
    expect(res.data.user.username).toBe("adrian");
    expect(res.etag).toBe('"v3"');
  });

  it("strips unknown fields (newer server)", async () => {
    server.use(http.get("*/v1/me", () => HttpResponse.json({ ...ME, future_field: 1 })));
    const res = await getMe();
    expect(res.data).not.toHaveProperty("future_field");
  });

  it("handles 204 No Content", async () => {
    server.use(http.post("*/v1/auth/logout", () => new HttpResponse(null, { status: 204 })));
    expect(await callEmpty((o) => client.POST("/v1/auth/logout", o))).toEqual({ status: 204 });
  });

  it("rejects malformed JSON", async () => {
    server.use(
      http.get(
        "*/v1/me",
        () =>
          new HttpResponse("{not json", {
            headers: { "Content-Type": "application/json", "X-Request-Id": "r1" },
          }),
      ),
    );
    const err = await failure(getMe());
    expect(err.detail).toEqual({ kind: "malformed", status: 200, requestId: "r1" });
    expect(err.transient).toBe(false);
  });

  it("rejects a body that does not match the schema", async () => {
    server.use(
      http.get("*/v1/me", () =>
        HttpResponse.json({ ...ME, user: { ...ME.user, role: "superuser" } }),
      ),
    );
    const err = await failure(getMe());
    expect(err.detail.kind).toBe("schema");
    expect(err.detail.kind === "schema" && err.detail.issues.join()).toContain("user.role");
  });

  it("rejects an empty 200 body", async () => {
    server.use(http.get("*/v1/me", () => new HttpResponse("", { status: 200 })));
    expect((await failure(getMe())).detail.kind).toBe("malformed");
  });
});

describe("error statuses", () => {
  it.each([
    [400, "validation_failed"],
    [401, "unauthenticated"],
    [403, "forbidden"],
    [404, "not_found"],
    [409, "conflict"],
    [412, "precondition_failed"],
    [428, "precondition_required"],
    [500, "internal"],
    [503, "unavailable"],
  ])("maps %i problem+json to a typed error", async (status, code) => {
    server.use(
      http.get("*/v1/me", () =>
        problem(status, code, { request_id: "req-9" }, { "X-Request-Id": "req-9" }),
      ),
    );
    const err = await failure(getMe());
    expect(err.status).toBe(status);
    expect(err.code).toBe(code);
    expect(err.detail.kind === "http" && err.detail.requestId).toBe("req-9");
    expect(err.transient).toBe(status >= 500);
  });

  it("exposes field errors from validation failures", async () => {
    server.use(
      http.get("*/v1/me", () =>
        problem(400, "validation_failed", {
          errors: [
            { field: "title", code: "too_long", message: "At most 200 characters" },
            { field: "slug", code: "pattern" },
          ],
        }),
      ),
    );
    const err = await failure(getMe());
    expect(err.fieldErrors()).toEqual({ title: "At most 200 characters", slug: "pattern" });
  });

  it("reads Retry-After on 429", async () => {
    server.use(http.get("*/v1/me", () => problem(429, "rate_limited", {}, { "Retry-After": "7" })));
    const err = await failure(getMe());
    expect(err.detail.kind === "http" && err.detail.retryAfterSeconds).toBe(7);
    expect(err.transient).toBe(true);
    expect(retryDelay(0, err)).toBe(7000);
  });

  it("synthesizes a problem for non-JSON error bodies (proxy HTML)", async () => {
    server.use(
      http.get(
        "*/v1/me",
        () =>
          new HttpResponse("<html>Bad Gateway</html>", { status: 502, statusText: "Bad Gateway" }),
      ),
    );
    const err = await failure(getMe());
    expect(err.status).toBe(502);
    expect(err.code).toBe("http_502");
    expect(err.transient).toBe(true);
  });

  it("synthesizes a problem for JSON error bodies that are not problem details", async () => {
    server.use(http.get("*/v1/me", () => HttpResponse.json({ message: "nope" }, { status: 418 })));
    const err = await failure(getMe());
    expect(err.code).toBe("http_418");
  });
});

describe("transport", () => {
  it("maps a network failure", async () => {
    server.use(http.get("*/v1/me", () => HttpResponse.error()));
    const err = await failure(getMe());
    expect(err.detail.kind).toBe("network");
    expect(err.transient).toBe(true);
  });

  it("times out", async () => {
    server.use(
      http.get("*/v1/slow", async () => {
        await delay(500);
        return HttpResponse.json({});
      }),
    );
    const err = await failure(policyFetch(new Request(`${window.location.origin}/v1/slow`), 50));
    expect(err.detail).toEqual({ kind: "timeout", afterMs: 50 });
  });

  it("reports caller cancellation as aborted, not as an error to show", async () => {
    server.use(
      http.get("*/v1/me", async () => {
        await delay(500);
        return HttpResponse.json(ME);
      }),
    );
    const controller = new AbortController();
    const pending = call(MeSchema, (o) => client.GET("/v1/me", o), controller.signal);
    controller.abort();
    expect((await failure(pending)).detail.kind).toBe("aborted");
  });

  it("sends the CSRF token on unsafe methods only, with same-origin credentials", async () => {
    // biome-ignore lint/suspicious/noDocumentCookie: simulates the server-set CSRF cookie.
    document.cookie = "__Host-vgames_csrf=tok-123; path=/; secure";
    const seen: { method: string; csrf: string | null; credentials: string }[] = [];
    server.use(
      http.all("*/v1/*", ({ request }) => {
        seen.push({
          method: request.method,
          csrf: request.headers.get("X-CSRF-Token"),
          credentials: request.credentials,
        });
        return request.method === "GET"
          ? HttpResponse.json(ME)
          : new HttpResponse(null, { status: 204 });
      }),
    );
    await getMe();
    await callEmpty((o) => client.POST("/v1/auth/logout", o));
    expect(seen).toEqual([
      { method: "GET", csrf: null, credentials: "same-origin" },
      { method: "POST", csrf: "tok-123", credentials: "same-origin" },
    ]);
  });
});

describe("retry policy", () => {
  const http4xx = new ApiError({
    kind: "http",
    status: 404,
    problem: { type: "x", title: "x", status: 404, code: "not_found" },
    retryAfterSeconds: null,
    requestId: null,
  });
  const http5xx = new ApiError({
    kind: "http",
    status: 503,
    problem: { type: "x", title: "x", status: 503, code: "unavailable" },
    retryAfterSeconds: null,
    requestId: null,
  });

  it("never retries 4xx, schema or malformed responses", () => {
    expect(shouldRetry(0, http4xx)).toBe(false);
    expect(
      shouldRetry(0, new ApiError({ kind: "schema", status: 200, issues: [], requestId: null })),
    ).toBe(false);
    expect(shouldRetry(0, new ApiError({ kind: "malformed", status: 200, requestId: null }))).toBe(
      false,
    );
    expect(shouldRetry(0, new Error("render bug"))).toBe(false);
  });

  it("retries network errors and 5xx twice with backoff", () => {
    expect(shouldRetry(0, http5xx)).toBe(true);
    expect(shouldRetry(1, new ApiError({ kind: "network" }))).toBe(true);
    expect(shouldRetry(2, http5xx)).toBe(false);
    expect(retryDelay(0, http5xx)).toBe(1000);
    expect(retryDelay(1, http5xx)).toBe(2000);
  });
});

describe("idempotency keys", () => {
  it("match the API pattern and are unique", () => {
    const a = newIdempotencyKey();
    expect(a).toMatch(/^[A-Za-z0-9_-]{16,128}$/);
    expect(newIdempotencyKey()).not.toBe(a);
  });
});
