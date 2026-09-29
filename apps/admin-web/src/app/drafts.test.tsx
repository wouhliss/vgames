// A3-T18: a session that ends in the middle of a form keeps the input through sign-in.
import { cleanup, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { type MockDb, packageId, type Role } from "../mocks/db";
import { editElsewhere } from "../mocks/packages";
import { renderAt } from "../test/render";
import { RESTORED_MESSAGE, signedInAgain, stashDrafts } from "./drafts";

const HARBOR = packageId(1);

/** Ends the session on the server: the next request answers 401. */
function expire(db: MockDb): Role {
  const role = db.role;
  if (!role) throw new Error("not signed in");
  db.role = null;
  return role;
}

/** The app went to sign-in and is waiting there, with `returnTo` to come back to. */
async function expectSignInFor(router: ReturnType<typeof renderAt>["router"], returnTo: string) {
  expect(await screen.findByRole("heading", { name: "Sign in" })).toBeInTheDocument();
  expect(router.state.location.pathname).toBe("/login");
  expect(new URLSearchParams(router.state.location.search).get("return_to")).toBe(returnTo);
}

/** Signing in again reloads the page (Discord redirect): fresh app, same tab storage and server. */
function signInAgain(db: MockDb, role: Role, path: string) {
  cleanup();
  signedInAgain();
  db.role = role;
  return renderAt(path, role, { existing: db });
}

describe("drafts across sign-in", () => {
  it("the package editor keeps its input, the version it was based on, and asks nothing", async () => {
    const first = renderAt(`/packages/${HARBOR}`);
    const summary = await screen.findByLabelText("Summary");
    await first.user.clear(summary);
    await first.user.type(summary, "Written before the session ended");
    const role = expire(first.db);
    await first.user.click(screen.getByRole("button", { name: "Save changes" }));
    // No "leave with unsaved changes?" question: the input is kept.
    await expectSignInFor(first.router, `/admin/packages/${HARBOR}`);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    const again = signInAgain(first.db, role, `/packages/${HARBOR}`);
    expect(await screen.findByLabelText("Summary")).toHaveValue("Written before the session ended");
    expect(screen.getByText(RESTORED_MESSAGE)).toBeInTheDocument();
    await again.user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
    expect(again.db.packages.find((p) => p.id === HARBOR)?.summary).toBe(
      "Written before the session ended",
    );
  });

  it("a change saved by someone else meanwhile is a conflict, not an overwrite", async () => {
    const first = renderAt(`/packages/${HARBOR}`);
    await first.user.type(await screen.findByLabelText("Summary"), " mine");
    const role = expire(first.db);
    await first.user.click(screen.getByRole("button", { name: "Save changes" }));
    await expectSignInFor(first.router, `/admin/packages/${HARBOR}`);
    editElsewhere(first.db, HARBOR, { summary: "Theirs" });

    const again = signInAgain(first.db, role, `/packages/${HARBOR}`);
    await screen.findByText(RESTORED_MESSAGE);
    await again.user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByText("Changed by someone else")).toBeInTheDocument();
    expect(again.db.packages.find((p) => p.id === HARBOR)?.summary).toBe("Theirs");
  });

  it("another account signing in on the same tab doesn't get the draft", async () => {
    const first = renderAt(`/packages/${HARBOR}`, "admin");
    await first.user.type(await screen.findByLabelText("Summary"), " private");
    expire(first.db);
    await first.user.click(screen.getByRole("button", { name: "Save changes" }));
    await expectSignInFor(first.router, `/admin/packages/${HARBOR}`);

    signInAgain(first.db, "owner", `/packages/${HARBOR}`);
    const summary = await screen.findByLabelText("Summary");
    expect(summary).not.toHaveValue(expect.stringContaining("private"));
    expect(screen.queryByText(RESTORED_MESSAGE)).not.toBeInTheDocument();
  });

  it("a draft is restored once", async () => {
    const first = renderAt("/packages/new");
    await first.user.type(await screen.findByLabelText("Title (required)"), "Night Ferry");
    const role = expire(first.db);
    await first.user.click(screen.getByRole("button", { name: "Create package" }));
    await expectSignInFor(first.router, "/admin/packages/new");

    signInAgain(first.db, role, "/packages/new");
    expect(await screen.findByLabelText("Title (required)")).toHaveValue("Night Ferry");
    expect(first.db.packages.some((p) => p.title === "Night Ferry")).toBe(false);

    signInAgain(first.db, role, "/packages/new");
    expect(await screen.findByLabelText("Title (required)")).toHaveValue("");
  });

  it("server settings keep the input and the version it was based on", async () => {
    const first = renderAt("/settings", "owner");
    const name = await screen.findByLabelText("Server name (required)");
    await first.user.clear(name);
    await first.user.type(name, "Late Night Games");
    await first.user.click(screen.getByRole("radio", { name: /Closed/ }));
    const role = expire(first.db);
    await first.user.click(screen.getByRole("button", { name: "Save settings" }));
    await expectSignInFor(first.router, "/admin/settings");

    const again = signInAgain(first.db, role, "/settings");
    expect(await screen.findByLabelText("Server name (required)")).toHaveValue("Late Night Games");
    expect(screen.getByRole("radio", { name: /Closed/ })).toBeChecked();
    expect(screen.getByText(RESTORED_MESSAGE)).toBeInTheDocument();
    await again.user.click(screen.getByRole("button", { name: "Save settings" }));
    await waitFor(() => expect(again.db.settings.name).toBe("Late Night Games"));
    expect(again.db.settings.registration_mode).toBe("closed");
  });

  it("the allowlist form keeps the id and note", async () => {
    const first = renderAt("/allowlist");
    await first.user.type(await screen.findByLabelText("Discord id"), "123456789012345678");
    await first.user.type(screen.getByLabelText("Note (optional)"), "Sam's brother");
    const role = expire(first.db);
    await first.user.click(screen.getByRole("button", { name: "Add" }));
    await expectSignInFor(first.router, "/admin/allowlist");

    signInAgain(first.db, role, "/allowlist");
    expect(await screen.findByLabelText("Discord id")).toHaveValue("123456789012345678");
    expect(screen.getByLabelText("Note (optional)")).toHaveValue("Sam's brother");
  });

  it("an untouched form leaves no draft", async () => {
    renderAt(`/packages/${HARBOR}`);
    await screen.findByLabelText("Summary");
    stashDrafts();
    expect(Object.keys(sessionStorage).filter((k) => k.startsWith("vgames.admin.draft:"))).toEqual(
      [],
    );
  });

  it("ignores drafts that are old, belong to someone else or don't match the form", async () => {
    const put = (key: string, value: unknown) =>
      sessionStorage.setItem(`vgames.admin.draft:${key}`, JSON.stringify(value));
    const db = renderAt("/allowlist").db;
    await screen.findByLabelText("Discord id");
    const user = db.users.find((u) => u.role === "admin")?.id ?? "";
    cleanup();

    put("allowlist-add", {
      user,
      at: Date.now() - 25 * 60 * 60 * 1000,
      value: { id: "1", note: "" },
    });
    renderAt("/allowlist", "admin", { existing: db });
    expect(await screen.findByLabelText("Discord id")).toHaveValue("");
    cleanup();

    put("allowlist-add", { user, at: Date.now(), value: { id: 42, note: null } });
    renderAt("/allowlist", "admin", { existing: db });
    expect(await screen.findByLabelText("Discord id")).toHaveValue("");
    cleanup();

    sessionStorage.setItem("vgames.admin.draft:allowlist-add", "{not json");
    renderAt("/allowlist", "admin", { existing: db });
    expect(await screen.findByLabelText("Discord id")).toHaveValue("");
    // Read once, then gone, even when unusable.
    expect(sessionStorage.getItem("vgames.admin.draft:allowlist-add")).toBeNull();
  });
});
