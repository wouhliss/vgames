import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "./errors";
import { checkImage, MAX_IMAGE_BYTES, uploadAsset } from "./upload";

const PNG = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
const JPEG = [0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0, 0, 0, 0, 0];
const WEBP = [...new TextEncoder().encode("RIFF"), 0, 0, 0, 0, ...new TextEncoder().encode("WEBP")];

const file = (bytes: number[], name = "x", size = 0) =>
  new File([new Uint8Array(bytes), new Uint8Array(size)], name);

describe("checkImage", () => {
  it("accepts JPEG, PNG and WebP by content", async () => {
    for (const bytes of [PNG, JPEG, WEBP])
      expect(await checkImage(file(bytes, "no-extension"))).toBeNull();
  });

  it("refuses other content, empty files and files over 10 MiB", async () => {
    expect(await checkImage(file([0x47, 0x49, 0x46, 0x38], "image.png"))).toMatch(/Only JPEG, PNG/);
    expect(await checkImage(new File([], "empty.png"))).toBe("The file is empty.");
    expect(await checkImage(file(PNG, "big.png", MAX_IMAGE_BYTES))).toMatch(/at most 10 MiB/);
    expect(await checkImage(file(PNG, "ok.png", MAX_IMAGE_BYTES - PNG.length))).toBeNull();
  });
});

/** A scriptable XMLHttpRequest. */
class FakeXhr {
  static last: FakeXhr | null = null;
  method = "";
  url = "";
  headers: Record<string, string> = {};
  body: unknown = null;
  status = 0;
  statusText = "";
  responseText = "";
  timeout = 0;
  responseHeaders = "";
  upload: {
    onprogress: ((e: { lengthComputable: boolean; loaded: number; total: number }) => void) | null;
  } = {
    onprogress: null,
  };
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  ontimeout: (() => void) | null = null;
  onabort: (() => void) | null = null;
  constructor() {
    FakeXhr.last = this;
  }
  open(method: string, url: string) {
    this.method = method;
    this.url = url;
  }
  setRequestHeader(name: string, value: string) {
    this.headers[name] = value;
  }
  getAllResponseHeaders() {
    return this.responseHeaders;
  }
  send(body: unknown) {
    this.body = body;
  }
  abort() {
    this.onabort?.();
  }
  respond(status: number, body: string, headers = "") {
    this.status = status;
    this.responseText = body;
    this.responseHeaders = headers;
    this.onload?.();
  }
}

const ASSET = {
  id: "01920000-0000-7000-8000-0000000a0101",
  kind: "cover",
  url: "/v1/assets/01920000-0000-7000-8000-0000000a0101",
  width: 600,
  height: 900,
  content_type: "image/png",
  source: "upload",
};

afterEach(() => {
  vi.unstubAllGlobals();
  // biome-ignore lint/suspicious/noDocumentCookie: resets the simulated CSRF cookie.
  document.cookie = "__Host-vgames_csrf=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/";
});

describe("uploadAsset", () => {
  it("posts multipart with the CSRF header, reports progress and validates the answer", async () => {
    vi.stubGlobal("XMLHttpRequest", FakeXhr);
    // biome-ignore lint/suspicious/noDocumentCookie: simulates the server-set CSRF cookie.
    document.cookie = "__Host-vgames_csrf=tok123; path=/; secure";
    const progress: number[] = [];
    const done = uploadAsset("pkg-1", "cover", file(PNG, "c.png"), (f) => progress.push(f));
    const xhr = FakeXhr.last;
    if (!xhr) throw new Error("no xhr");
    expect(xhr.method).toBe("POST");
    expect(xhr.url).toBe("/v1/admin/packages/pkg-1/assets");
    expect(xhr.headers["X-CSRF-Token"]).toBe("tok123");
    expect(xhr.body).toBeInstanceOf(FormData);
    expect((xhr.body as FormData).get("kind")).toBe("cover");
    xhr.upload.onprogress?.({ lengthComputable: true, loaded: 50, total: 100 });
    xhr.respond(201, JSON.stringify(ASSET));
    expect((await done).id).toBe(ASSET.id);
    expect(progress).toEqual([0.5, 1]);
  });

  it("turns problem+json into a typed error (413, 415)", async () => {
    vi.stubGlobal("XMLHttpRequest", FakeXhr);
    for (const [status, code] of [
      [413, "payload_too_large"],
      [415, "unsupported_media_type"],
    ] as const) {
      const done = uploadAsset("p", "logo", file(PNG), () => {});
      FakeXhr.last?.respond(
        status,
        JSON.stringify({ type: `urn:vgames:problem:${code}`, title: "No", status, code }),
        "Content-Type: application/problem+json\r\nX-Request-Id: r1",
      );
      const error = await done.catch((e: unknown) => e);
      expect(error).toBeInstanceOf(ApiError);
      expect((error as ApiError).status).toBe(status);
      expect((error as ApiError).code).toBe(code);
    }
  });

  it("rejects malformed and off-contract answers, network errors and timeouts", async () => {
    vi.stubGlobal("XMLHttpRequest", FakeXhr);
    const kinds: string[] = [];
    for (const act of [
      (x: FakeXhr) => x.respond(201, "<html>"),
      (x: FakeXhr) => x.respond(201, JSON.stringify({ ...ASSET, width: "wide" })),
      (x: FakeXhr) => x.onerror?.(),
      (x: FakeXhr) => x.ontimeout?.(),
    ]) {
      const done = uploadAsset("p", "logo", file(PNG), () => {});
      const xhr = FakeXhr.last;
      if (!xhr) throw new Error("no xhr");
      act(xhr);
      kinds.push(((await done.catch((e: unknown) => e)) as ApiError).detail.kind);
    }
    expect(kinds).toEqual(["malformed", "schema", "network", "timeout"]);
  });

  it("aborts when the caller cancels", async () => {
    vi.stubGlobal("XMLHttpRequest", FakeXhr);
    const controller = new AbortController();
    const done = uploadAsset("p", "logo", file(PNG), () => {}, controller.signal);
    controller.abort();
    expect(((await done.catch((e: unknown) => e)) as ApiError).detail.kind).toBe("aborted");
  });
});
