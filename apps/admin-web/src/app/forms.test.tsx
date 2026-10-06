// A3-T18: every admin form under every way a save can fail, double submits, and very long unicode
// input. A failed save says what happened, keeps what was typed, and changes nothing on screen that
// the server didn't confirm.
import { screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { describe, expect, it } from "vitest";
import { makePackage, packageId, type Role } from "../mocks/db";
import { problem } from "../mocks/handlers";
import { renderAt } from "../test/render";
import { server } from "../test/setup";

const HARBOR = packageId(1);

interface Form {
  name: string;
  path: string;
  role: Role;
  method: "POST" | "PATCH";
  endpoint: string;
  /** Types the input; returns what the kept field should show. */
  fill: (user: UserEvent) => Promise<{ field: HTMLElement; value: string }>;
  submit: string;
}

const FORMS: Form[] = [
  {
    name: "create package",
    path: "/packages/new",
    role: "admin",
    method: "POST",
    endpoint: "*/v1/admin/packages",
    fill: async (user) => {
      const field = await screen.findByLabelText("Title (required)");
      await user.type(field, "Night Ferry");
      return { field, value: "Night Ferry" };
    },
    submit: "Create package",
  },
  {
    name: "package editor",
    path: `/packages/${HARBOR}`,
    role: "admin",
    method: "PATCH",
    endpoint: "*/v1/admin/packages/:id",
    fill: async (user) => {
      const field = await screen.findByLabelText("Summary");
      await user.clear(field);
      await user.type(field, "Mine");
      return { field, value: "Mine" };
    },
    submit: "Save changes",
  },
  {
    name: "server settings",
    path: "/settings",
    role: "owner",
    method: "PATCH",
    endpoint: "*/v1/admin/settings",
    fill: async (user) => {
      const field = await screen.findByLabelText("Server name (required)");
      await user.clear(field);
      await user.type(field, "Late Night Games");
      return { field, value: "Late Night Games" };
    },
    submit: "Save settings",
  },
  {
    name: "allowlist",
    path: "/allowlist",
    role: "admin",
    method: "POST",
    endpoint: "*/v1/admin/allowlist",
    fill: async (user) => {
      const field = await screen.findByLabelText("Discord id");
      await user.type(field, "123456789012345678");
      return { field, value: "123456789012345678" };
    },
    submit: "Add",
  },
];

interface Failure {
  status: string;
  respond: () => Response;
  /** Words the user sees (from the page or the shared error view). */
  says: RegExp;
}

const FAILURES: Failure[] = [
  {
    status: "409",
    respond: () => problem(409, "conflict", "Conflict", { detail: "It clashes with a change." }),
    says: /Conflict|clashes/,
  },
  {
    status: "412",
    respond: () => problem(412, "precondition_failed", "Precondition failed"),
    says: /Changed by someone else/,
  },
  {
    status: "428",
    respond: () => problem(428, "precondition_required", "Precondition required"),
    says: /Reload required/,
  },
  {
    status: "429",
    respond: () => {
      const response = problem(429, "rate_limited", "Too many requests");
      response.headers.set("Retry-After", "12");
      return response;
    },
    says: /Too many requests/,
  },
  { status: "500", respond: () => problem(500, "internal", "Internal"), says: /Server error/ },
  { status: "offline", respond: () => HttpResponse.error(), says: /Can't reach the server/ },
];

/** A submit button while its request runs. */
const BUSY = /^(Saving|Creating|Adding)…$/;

function countWrites(method: string) {
  const seen: string[] = [];
  server.events.on("request:start", ({ request }) => {
    if (request.method === method) seen.push(new URL(request.url).pathname);
  });
  return seen;
}

describe.each(FORMS)("$name", (form) => {
  it.each(FAILURES)("a $status on save says so and keeps the input", async (failure) => {
    const handler = form.method === "POST" ? http.post : http.patch;
    const { user } = renderAt(form.path, form.role, {
      overrides: [handler(form.endpoint, failure.respond)],
    });
    const { field, value } = await form.fill(user);
    await user.click(screen.getByRole("button", { name: form.submit }));
    const said = await screen.findAllByText(failure.says);
    expect(said.length).toBeGreaterThan(0);
    expect(field).toHaveValue(value);
    expect(document.body.textContent).not.toMatch(/undefined|\[object Object\]/);
    // Nothing claims success.
    expect(screen.queryByText(/^Saved\.$/)).not.toBeInTheDocument();
  });

  it("a double click sends one request", async () => {
    const writes = countWrites(form.method);
    const { user } = renderAt(form.path, form.role);
    await form.fill(user);
    await user.dblClick(screen.getByRole("button", { name: form.submit }));
    // Settled: the write went out and no button still shows a busy label ("Saving…").
    await waitFor(() => expect(writes.length).toBeGreaterThan(0));
    await waitFor(() => expect(screen.queryAllByRole("button", { name: BUSY })).toHaveLength(0));
    expect(writes).toHaveLength(1);
  });
});

// Mixed scripts, emoji with modifiers and combining marks: counted as the server counts them.
const MIXED = "Ωmega 🎮🏽 ليلة été 東京 ";

function repeatTo(points: number): string {
  const unit = [...MIXED];
  return Array.from({ length: points }, (_, i) => unit[i % unit.length]).join("");
}

describe("very long and unicode input", () => {
  it("a summary at its 500-character limit saves exactly as typed", async () => {
    const summary = repeatTo(500);
    const { user, db } = renderAt(`/packages/${HARBOR}`);
    const field = await screen.findByLabelText("Summary");
    await user.clear(field);
    // Typing 500 characters one key at a time is slow; paste like a user would.
    field.focus();
    await user.paste(summary);
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
    expect(db.packages.find((p) => p.id === HARBOR)?.summary).toBe(summary.trim());
  });

  it("one character over the limit is refused before sending", async () => {
    const writes = countWrites("PATCH");
    const { user } = renderAt(`/packages/${HARBOR}`);
    const field = await screen.findByLabelText("Summary");
    await user.clear(field);
    field.focus();
    await user.paste(`${repeatTo(500).trim()}x`.padEnd(501, "x"));
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(field).toHaveAttribute("aria-invalid", "true");
    expect(writes).toHaveLength(0);
  });

  it("long titles in lists and headings are shown whole, as text", async () => {
    const title = `${repeatTo(190)}<b>x</b>`;
    renderAt("/packages", "admin", {
      db: (db) => {
        db.packages = [makePackage(1, { title })];
      },
    });
    const link = await screen.findByRole("link", { name: title });
    expect(link).toHaveAttribute("dir", "auto");
    expect(link.querySelector("b")).toBeNull();
  });

  it("names from the server in right-to-left scripts keep their direction in tables", async () => {
    renderAt("/users", "owner", {
      db: (db) => {
        const first = db.users[2];
        if (first) first.display_name = "ليلى الطويلة جدًا ".repeat(8);
      },
    });
    const table = await screen.findByRole("table");
    const header = within(table).getAllByRole("rowheader")[2];
    expect(header).toHaveAttribute("dir", "auto");
  });
});
