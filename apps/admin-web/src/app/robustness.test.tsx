// A3-T18: every admin page in every way its data can fail to arrive. Each page's reads all fail the
// same way; the page must say what happened in words, offer the right action, and never crash or
// show raw values. Writes (409, 412, 428, double submits) are covered per form in their suites and
// in forms.test.tsx.
import { screen, within } from "@testing-library/react";
import { delay, HttpResponse, http } from "msw";
import type { ReactNode } from "react";
import { describe, expect, it } from "vitest";
import { type MockDb, packageId } from "../mocks/db";
import { problem } from "../mocks/handlers";
import { renderAt } from "../test/render";
import { inProcessWorkers } from "../test/workers";
import { UploadDepsContext } from "../upload/context";
import { memoryStore } from "../upload/store";

/** The compatibility page signs in workers; tests run them in process. */
function wrapper({ children }: { children: ReactNode }) {
  return (
    <UploadDepsContext.Provider value={{ workers: inProcessWorkers(), store: memoryStore }}>
      {children}
    </UploadDepsContext.Provider>
  );
}

const HARBOR = packageId(1);

interface Page {
  path: string;
  /** Makes the page's lists empty, and the words that say so. */
  empty?: { db: (db: MockDb) => void; text: string | RegExp; path?: string };
}

const PAGES: Page[] = [
  { path: "/packages", empty: { db: (db) => (db.packages = []), text: "No packages yet." } },
  { path: "/packages/new" },
  { path: `/packages/${HARBOR}` },
  {
    path: `/packages/${HARBOR}/metadata`,
    empty: {
      db: (db) => {
        db.candidates = {};
        db.metadataJobs = {};
      },
      text: "No lookup has run for this package yet.",
    },
  },
  { path: `/packages/${HARBOR}/images` },
  {
    path: `/packages/${HARBOR}/versions`,
    empty: { db: (db) => (db.versions = {}), text: "No versions yet. Upload the first one." },
  },
  {
    path: `/packages/${HARBOR}/compatibility`,
    empty: { db: (db) => (db.compat = {}), text: /^No profile yet/ },
  },
  { path: "/users", empty: { db: () => {}, text: "No users match.", path: "/users?q=zzzz" } },
  {
    path: "/allowlist",
    empty: { db: (db) => (db.allowlist = []), text: "Nobody is on the allowlist yet." },
  },
  { path: "/settings" },
  {
    path: "/trust",
    empty: { db: (db) => (db.trust.keys = []), text: /^No publisher keys yet/ },
  },
  { path: "/jobs", empty: { db: (db) => (db.jobs = []), text: "No jobs match." } },
  { path: "/audit", empty: { db: (db) => (db.audit = []), text: "No entries match." } },
];

/** Every read but the session check. */
const DATA_GET = /^(?!.*\/v1\/me(?:\?.*)?$).*\/(?:v1|\.well-known)\//;

interface State {
  name: string;
  respond: () => Response | Promise<Response>;
  /** Also fail `GET /v1/me` (a real 401 ends the session for every request). */
  session?: boolean;
  expect: RegExp;
}

const STATES: State[] = [
  {
    name: "401",
    respond: () => problem(401, "unauthenticated", "Unauthenticated"),
    session: true,
    expect: /^Sign in$/,
  },
  { name: "403", respond: () => problem(403, "forbidden", "Forbidden"), expect: /^Not allowed$/ },
  { name: "404", respond: () => problem(404, "not_found", "Not found"), expect: /^Not found$/ },
  {
    name: "429",
    respond: () => {
      const response = problem(429, "rate_limited", "Too many requests");
      response.headers.set("Retry-After", "30");
      return response;
    },
    expect: /^Too many requests$/,
  },
  {
    name: "500",
    respond: () => problem(500, "internal", "Internal error"),
    expect: /^Server error$/,
  },
  {
    name: "a proxy's HTML error page",
    respond: () => new HttpResponse("<html>Bad gateway</html>", { status: 502 }),
    expect: /^Server error$/,
  },
  { name: "offline", respond: () => HttpResponse.error(), expect: /^Can't reach the server$/ },
  {
    name: "a body that isn't JSON",
    respond: () => new HttpResponse("<html>ok</html>", { status: 200 }),
    expect: /^Unexpected response$/,
  },
  {
    name: "a body from another server version",
    respond: () => HttpResponse.json({ unexpected: true }),
    expect: /^Unexpected response$/,
  },
];

/** No raw values leak into the page when something fails. */
function expectNoRawValues() {
  const text = document.body.textContent ?? "";
  expect(text).not.toMatch(/undefined|\[object Object\]|NaN|null(?![a-z])/);
}

// Pages without reads of their own (the create form) only load the session.
const withReads = PAGES.filter((p) => p.path !== "/packages/new");

describe.each(withReads)("$path", (page) => {
  it.each(STATES)("says what happened on $name", async (state) => {
    const failing = [http.get(DATA_GET, state.respond)];
    if (state.session) failing.push(http.get("*/v1/me", state.respond));
    renderAt(page.path, "owner", { overrides: failing, wrapper });
    const found = await screen.findAllByText(state.expect, {}, { timeout: 3000 });
    expect(found.length).toBeGreaterThan(0);
    expectNoRawValues();
  });

  it("shows that it is loading while the server is slow", async () => {
    renderAt(page.path, "owner", {
      wrapper,
      overrides: [
        http.get(DATA_GET, async () => {
          await delay("infinite");
          return new Response();
        }),
      ],
    });
    const status = await screen.findAllByText(/^(Loading|Checking)/);
    expect(status.length).toBeGreaterThan(0);
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  if (page.empty) {
    const empty = page.empty;
    it("says when there is nothing to show", async () => {
      renderAt(empty.path ?? page.path, "owner", { db: empty.db, wrapper });
      const found = await screen.findAllByText(empty.text);
      expect(found.length).toBeGreaterThan(0);
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
      expectNoRawValues();
    });
  }
});

describe("a failed read can be retried", () => {
  it("Retry asks again and shows the page when the server is back", async () => {
    let down = true;
    const { user } = renderAt("/jobs", "owner", {
      overrides: [
        http.get("*/v1/admin/jobs", () =>
          down ? problem(503, "unavailable", "Unavailable") : undefined,
        ),
      ],
    });
    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText("Server error")).toBeInTheDocument();
    down = false;
    await user.click(within(alert).getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("table")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("a 429 counts down its Retry-After before Retry is offered", async () => {
    renderAt("/audit", "owner", {
      overrides: [
        http.get("*/v1/admin/audit-log", () => {
          const response = problem(429, "rate_limited", "Too many requests");
          response.headers.set("Retry-After", "30");
          return response;
        }),
      ],
    });
    const button = await screen.findByRole("button", { name: /^Retry in \d+ s$/ });
    expect(button).toBeDisabled();
  });
});
