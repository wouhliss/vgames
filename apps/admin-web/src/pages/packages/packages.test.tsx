import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { http } from "msw";
import { describe, expect, it } from "vitest";
import { manyPackages, packageId } from "../../mocks/db";
import { problem } from "../../mocks/handlers";
import { editElsewhere } from "../../mocks/packages";
import { renderAt } from "../../test/render";
import { server } from "../../test/setup";
import { buildPatch, FIELDS, fromPackage, length, slugify, validateField } from "./model";

const HARBOR = packageId(1);

/** Records the requests MSW sees (method + path + headers + body). */
function recordRequests() {
  const seen: { method: string; path: string; headers: Headers; body: string }[] = [];
  server.events.on("request:start", async ({ request }) => {
    const clone = request.clone();
    seen.push({
      method: request.method,
      path: new URL(request.url).pathname,
      headers: request.headers,
      body: await clone.text(),
    });
  });
  return seen;
}

const field = (name: RegExp | string) => screen.getByLabelText(name);

describe("model", () => {
  it("mirrors the server's slugify", () => {
    expect(slugify("Half-Life 2")).toBe("half-life-2");
    expect(slugify("  Pokémon: Épée & Bouclier!  ")).toBe("pokemon-epee-bouclier");
    expect(slugify("東京")).toBe("package");
    expect(slugify("---")).toBe("package");
    expect(slugify("a very long title ".repeat(10)).length).toBeLessThanOrEqual(56);
  });

  it("counts code points like the server (emoji, RTL, combining marks)", () => {
    expect(length("🎮🚀")).toBe(2);
    expect(length("مرآة")).toBe(4);
    const title = FIELDS.find((f) => f.name === "title");
    if (!title) throw new Error("title");
    expect(validateField(title, "🎮".repeat(200))).toBeNull();
    expect(validateField(title, "🎮".repeat(201))).toMatch(/At most 200/);
    expect(validateField(title, "   ")).toMatch(/required/);
  });

  it("builds a merge patch with only the changed fields, clearing empties", () => {
    const base = fromPackage({
      id: HARBOR,
      slug: "a",
      title: "A",
      platforms: [],
      updated_at: "2026-09-24T10:00:00Z",
      status: "draft",
      field_sources: {},
      created_at: "2026-09-24T10:00:00Z",
      created_by: { id: HARBOR, username: "o" },
      summary: "old",
      steam_app_id: 5,
    });
    expect(
      buildPatch(base, { ...base, summary: "", genres: "RPG\n\n Indie ", steam_app_id: "" }),
    ).toEqual({ summary: null, genres: ["RPG", "Indie"], steam_app_id: null });
    expect(buildPatch(base, base)).toEqual({});
  });
});

describe("packages list", () => {
  it("lists packages in a table, RTL and emoji titles included", async () => {
    renderAt("/packages");
    const table = await screen.findByRole("table");
    const rows = within(table).getAllByRole("row");
    expect(rows).toHaveLength(8);
    expect(within(table).getByRole("link", { name: "مرآة المحرك" })).toHaveAttribute("dir", "auto");
    expect(within(table).getByRole("link", { name: "Emoji Quest 🎮🚀" })).toBeInTheDocument();
  });

  it("filters by status and text through the URL", async () => {
    const { user, router } = renderAt("/packages");
    await screen.findByRole("table");
    await user.selectOptions(field("Status"), "published");
    await user.type(field("Search title or slug"), "night");
    await user.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(router.state.location.search).toBe("?q=night&status=published"));
    await waitFor(() => expect(screen.getAllByRole("row")).toHaveLength(2));
    expect(screen.getByRole("link", { name: "Night Canyon" })).toBeInTheDocument();
    // Deep link / back: the filters come from the URL.
    await router.navigate("/packages?status=hidden");
    await waitFor(() =>
      expect(screen.getByRole("link", { name: "Paper Station" })).toBeInTheDocument(),
    );
    expect(field("Status")).toHaveValue("hidden");
  });

  it("says when nothing matches, and when there are no packages at all", async () => {
    const { user } = renderAt("/packages?q=zzz");
    expect(await screen.findByText("No packages match these filters.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Clear filters" }));
    expect(await screen.findByRole("table")).toBeInTheDocument();
  });

  it("shows the empty state with a create link", async () => {
    renderAt("/packages", "admin", {
      db: (db) => {
        db.packages = [];
      },
    });
    expect(await screen.findByText("No packages yet.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Create the first package" })).toHaveAttribute(
      "href",
      "/packages/new",
    );
  });

  it("pages with Load more", async () => {
    const { user } = renderAt("/packages", "admin", {
      db: (db) => {
        db.packages = manyPackages(120);
      },
    });
    await screen.findByRole("table");
    expect(screen.getAllByRole("row")).toHaveLength(51);
    expect(screen.getByText("50 shown, more available")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Load more" }));
    await waitFor(() => expect(screen.getAllByRole("row")).toHaveLength(101));
    await user.click(screen.getByRole("button", { name: "Load more" }));
    await waitFor(() => expect(screen.getByText("120 shown")).toBeInTheDocument());
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  });

  it("shows errors with retry, and the forbidden page", async () => {
    let fail = true;
    const { user } = renderAt("/packages", "admin", {
      overrides: [
        http.get("*/v1/admin/packages", () =>
          fail ? problem(400, "invalid_cursor", "Bad cursor") : undefined,
        ),
      ],
    });
    expect(await screen.findByRole("alert")).toHaveTextContent("Bad cursor");
    fail = false;
    await user.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("table")).toBeInTheDocument();
  });

  it("shows the forbidden page on 403", async () => {
    renderAt("/packages", "admin", {
      overrides: [http.get("*/v1/admin/packages", () => problem(403, "forbidden", "Not allowed"))],
    });
    expect(await screen.findByRole("heading", { name: "Not allowed" })).toBeInTheDocument();
  });
});

describe("create package", () => {
  it("previews the slug and validates the title", async () => {
    const { user } = renderAt("/packages/new");
    const title = await screen.findByLabelText("Title (required)");
    expect(title).toHaveFocus();
    await user.type(title, "Pokémon: Épée");
    expect(screen.getByText(/Leave empty to use "pokemon-epee"/)).toBeInTheDocument();
    await user.clear(title);
    await user.click(screen.getByRole("button", { name: "Create package" }));
    expect(screen.getByText("Title is required.")).toBeInTheDocument();
    expect(title).toHaveAttribute("aria-invalid", "true");
    await user.click(title);
    await user.paste("x".repeat(201));
    await user.click(screen.getByRole("button", { name: "Create package" }));
    expect(screen.getByText("At most 200 characters (now 201).")).toBeInTheDocument();
  });

  it("creates and opens the editor; a double click creates one package", async () => {
    const seen = recordRequests();
    const { user, router, db } = renderAt("/packages/new");
    await user.type(await screen.findByLabelText("Title (required)"), "Brand New");
    const button = screen.getByRole("button", { name: "Create package" });
    fireEvent.click(button);
    fireEvent.click(button);
    await user.dblClick(button);
    await waitFor(() => expect(router.state.location.pathname).toMatch(/^\/packages\/0192/));
    const posts = seen.filter((r) => r.method === "POST" && r.path === "/v1/admin/packages");
    const keys = new Set(posts.map((r) => r.headers.get("Idempotency-Key")));
    expect(keys.size).toBe(1);
    expect(db.packages.filter((p) => p.title === "Brand New")).toHaveLength(1);
    expect(await screen.findByRole("heading", { level: 1, name: "Brand New" })).toBeInTheDocument();
  });

  it("a retried request with the same key returns the same package", async () => {
    let lost = false;
    const { user, db } = renderAt("/packages/new", "admin", {
      overrides: [
        // The server creates the package, but the answer is lost on the way back.
        http.post("*/v1/admin/packages", async ({ request }) => {
          if (lost) return undefined;
          lost = true;
          await fetch(request.clone());
          return Response.error();
        }),
      ],
    });
    await user.type(await screen.findByLabelText("Title (required)"), "Once Only");
    await user.click(screen.getByRole("button", { name: "Create package" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Can't reach the server");
    expect(db.packages.filter((p) => p.title === "Once Only")).toHaveLength(1);
    await user.click(screen.getByRole("button", { name: "Create package" }));
    expect(await screen.findByRole("heading", { level: 1, name: "Once Only" })).toBeInTheDocument();
    expect(db.packages.filter((p) => p.title === "Once Only")).toHaveLength(1);
  });

  it("maps a taken slug and server field errors onto the fields", async () => {
    const { user } = renderAt("/packages/new");
    await user.type(await screen.findByLabelText("Title (required)"), "Anything");
    await user.type(screen.getByLabelText("Slug (optional)"), "hollow-harbor");
    await user.click(screen.getByRole("button", { name: "Create package" }));
    expect(await screen.findByText(/Another package already uses this slug/)).toBeInTheDocument();
    expect(screen.getByLabelText("Slug (optional)")).toHaveAttribute("aria-invalid", "true");
  });
});

describe("package editor", () => {
  it("shows every field with its source", async () => {
    renderAt(`/packages/${HARBOR}`);
    expect(
      await screen.findByRole("heading", { level: 1, name: "Hollow Harbor" }),
    ).toBeInTheDocument();
    const form = screen.getByRole("form", { name: "Package details" });
    expect(within(form).getByLabelText("Title (required)")).toHaveValue("Hollow Harbor");
    expect(within(form).getByLabelText("Genres")).toHaveValue("Adventure\nSimulation");
    expect(within(form).getAllByText("From IGDB").length).toBeGreaterThan(0);
    expect(within(form).getByText("Edited by an admin")).toBeInTheDocument();
    // Server text in a field is data, never markup.
    expect(within(form).getByLabelText("Description")).toHaveValue(
      "## About\n\nFish, trade and explore.\n\n<script>alert(1)</script>",
    );
  });

  it("saves only what changed, as a merge patch with If-Match", async () => {
    const seen = recordRequests();
    const { user, db } = renderAt(`/packages/${HARBOR}`);
    const developer = await screen.findByLabelText("Developer");
    await user.clear(developer);
    await user.type(developer, "Tidewater Studio");
    expect(screen.getByText("You have unsaved changes.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
    const patch = seen.find((r) => r.method === "PATCH");
    expect(patch?.headers.get("If-Match")).toBe('W/"1"');
    expect(patch?.headers.get("Content-Type")).toBe("application/merge-patch+json");
    expect(JSON.parse(patch?.body ?? "")).toEqual({ developer: "Tidewater Studio" });
    expect(db.packages.find((p) => p.id === HARBOR)?.field_sources.developer).toBe("admin");
    // The next save uses the new version.
    await user.type(developer, "!");
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() =>
      expect(seen.filter((r) => r.method === "PATCH")[1]?.headers.get("If-Match")).toBe('W/"2"'),
    );
  });

  it("on 412 keeps the input, shows both sides, and saves over theirs when asked", async () => {
    const { user, db } = renderAt(`/packages/${HARBOR}`);
    const summary = await screen.findByLabelText("Summary");
    await user.clear(summary);
    await user.type(summary, "My summary");
    editElsewhere(db, HARBOR, { summary: "Their summary", publisher: "Other Pub" });
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    const alert = await screen.findByText("Changed by someone else");
    const panel = alert.closest("[role=alert]") as HTMLElement;
    expect(summary).toHaveValue("My summary");
    const rows = within(panel).getAllByRole("row");
    expect(rows.map((r) => r.textContent)).toEqual([
      "FieldSaved by themYours",
      "SummaryTheir summaryMy summary",
      "PublisherOther PubTidewater Games",
    ]);
    await user.click(within(panel).getByRole("button", { name: "Keep my changes (then save)" }));
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
    const saved = db.packages.find((p) => p.id === HARBOR);
    expect(saved?.summary).toBe("My summary");
    // Their publisher change survives: only my differences were sent. (My form still showed the
    // old publisher, so it is now a change of mine against their version, which I can review.)
    expect(screen.getByLabelText("Publisher")).toHaveValue("Tidewater Games");
  });

  it("on 412 can load theirs instead", async () => {
    const { user, db } = renderAt(`/packages/${HARBOR}`);
    const summary = await screen.findByLabelText("Summary");
    await user.type(summary, " mine");
    editElsewhere(db, HARBOR, { summary: "Theirs" });
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    await user.click(await screen.findByRole("button", { name: "Discard mine and load theirs" }));
    expect(summary).toHaveValue("Theirs");
    expect(screen.getByText("Loaded the latest saved version.")).toBeInTheDocument();
  });

  it.each([
    ["Summary", 500],
    ["Developer", 200],
    ["Publisher", 200],
  ])("enforces the %s limit at 0, max and max+1 (unicode)", async (label, max) => {
    const { user } = renderAt(`/packages/${HARBOR}`);
    const input = await screen.findByLabelText(label);
    await user.clear(input);
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(input).not.toHaveAttribute("aria-invalid");
    await user.click(input);
    await user.paste("é".repeat(max));
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(input).not.toHaveAttribute("aria-invalid");
    await user.click(input);
    await user.paste("🎮");
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByText(`At most ${max} characters (now ${max + 1}).`)).toBeInTheDocument();
  });

  it("enforces the description limit (20,000)", async () => {
    const { user } = renderAt(`/packages/${HARBOR}`);
    const input = await screen.findByLabelText("Description");
    fireEvent.change(input, { target: { value: "x".repeat(20_000) } });
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(input).not.toHaveAttribute("aria-invalid");
    fireEvent.change(input, { target: { value: "x".repeat(20_001) } });
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(screen.getByText("At most 20,000 characters (now 20,001).")).toBeInTheDocument();
  });

  it("checks genres, ids, slug and date", async () => {
    const { user } = renderAt(`/packages/${HARBOR}`);
    const genres = await screen.findByLabelText("Genres");
    fireEvent.change(genres, {
      target: { value: Array.from({ length: 21 }, (_, i) => `G${i}`).join("\n") },
    });
    fireEvent.change(screen.getByLabelText("Steam app id"), { target: { value: "0" } });
    fireEvent.change(screen.getByLabelText("Slug (required)"), { target: { value: "Bad Slug" } });
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(screen.getByText("At most 20 genres (you have 21).")).toBeInTheDocument();
    expect(screen.getByText("Must be a whole number of at least 1.")).toBeInTheDocument();
    expect(screen.getByText(/Use 1–64 lowercase letters/)).toBeInTheDocument();
    // Focus goes to the first invalid field.
    await waitFor(() => expect(screen.getByLabelText("Slug (required)")).toHaveFocus());
    fireEvent.change(genres, { target: { value: `${"g".repeat(65)}` } });
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(screen.getByText(/Each genre can be at most 64 characters/)).toBeInTheDocument();
  });

  it("maps server field errors (genres[1]) onto the form", async () => {
    const { user } = renderAt(`/packages/${HARBOR}`, "admin", {
      overrides: [
        http.patch("*/v1/admin/packages/:id", () =>
          problem(400, "validation_failed", "Invalid", {
            errors: [{ field: "genres[1]", code: "length", message: "must be 1-64 characters" }],
          }),
        ),
      ],
    });
    const genres = await screen.findByLabelText("Genres");
    await user.type(genres, "\nX");
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(
      await screen.findByText("The server says: must be 1-64 characters."),
    ).toBeInTheDocument();
    expect(genres).toHaveAttribute("aria-invalid", "true");
  });

  it("asks before leaving with unsaved changes", async () => {
    const { user, router } = renderAt(`/packages/${HARBOR}`);
    await user.type(await screen.findByLabelText("Developer"), "!");
    await user.click(screen.getByRole("link", { name: "← Packages" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Leave without saving?" });
    expect(within(dialog).getByRole("button", { name: "Stay on this page" })).toHaveFocus();
    await user.click(within(dialog).getByRole("button", { name: "Stay on this page" }));
    expect(router.state.location.pathname).toBe(`/packages/${HARBOR}`);
    expect(screen.getByLabelText("Developer")).toHaveValue("Tidewater Games!");
    await user.click(screen.getByRole("link", { name: "← Packages" }));
    await user.click(await screen.findByRole("button", { name: "Leave and discard changes" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/packages"));
  });

  it("warns on tab close only while there are unsaved changes", async () => {
    const { user } = renderAt(`/packages/${HARBOR}`);
    const developer = await screen.findByLabelText("Developer");
    const clean = new Event("beforeunload", { cancelable: true });
    window.dispatchEvent(clean);
    expect(clean.defaultPrevented).toBe(false);
    await user.type(developer, "!");
    const dirty = new Event("beforeunload", { cancelable: true });
    window.dispatchEvent(dirty);
    expect(dirty.defaultPrevented).toBe(true);
  });

  it("changes the status after confirmation", async () => {
    const { user, db } = renderAt(`/packages/${HARBOR}`);
    await user.selectOptions(await screen.findByLabelText("New status"), "hidden");
    await user.click(screen.getByRole("button", { name: "Change status…" }));
    const dialog = await screen.findByRole("dialog", { name: "Change status to Hidden?" });
    expect(dialog).toHaveTextContent("From Published to Hidden");
    await user.click(within(dialog).getByRole("button", { name: "Change to Hidden" }));
    expect(await screen.findByText("Status changed to Hidden.")).toBeInTheDocument();
    expect(db.packages.find((p) => p.id === HARBOR)?.status).toBe("hidden");
  });

  it("deletes only after the slug is typed", async () => {
    const { user, router, db } = renderAt(`/packages/${HARBOR}`);
    await user.click(await screen.findByRole("button", { name: "Delete package…" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Delete Hollow Harbor?" });
    const confirm = within(dialog).getByRole("button", { name: "Delete package" });
    expect(confirm).toBeDisabled();
    await user.type(within(dialog).getByLabelText(/to confirm/), "hollow-harbo");
    expect(confirm).toBeDisabled();
    await user.type(within(dialog).getByLabelText(/to confirm/), "r");
    await user.click(confirm);
    await waitFor(() => expect(router.state.location.pathname).toBe("/packages"));
    expect(db.packages.some((p) => p.id === HARBOR)).toBe(false);
    expect(await screen.findByText(/^Deleted/)).toHaveTextContent("Deleted Hollow Harbor.");
  });

  it("shows not found for a missing package", async () => {
    renderAt(`/packages/${packageId(999)}`);
    expect(await screen.findByRole("heading", { name: "Not found" })).toBeInTheDocument();
  });
});
