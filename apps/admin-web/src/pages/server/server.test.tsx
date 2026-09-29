import { screen, waitFor, within } from "@testing-library/react";
import { http } from "msw";
import { describe, expect, it } from "vitest";
import { IDS } from "../../mocks/db";
import { problem } from "../../mocks/handlers";
import { renderAt } from "../../test/render";

const row = (name: RegExp | string) => screen.getByRole("row", { name });

describe("users", () => {
  it("lists users and filters by role through the URL", async () => {
    const { user, router } = renderAt("/users");
    const table = await screen.findByRole("table", { name: "Users" });
    expect(within(table).getAllByRole("row")).toHaveLength(6);
    expect(row(/gone/)).toHaveTextContent("Disabled (Asked to leave)");
    await user.selectOptions(screen.getByLabelText("Role"), "admin");
    await user.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(router.state.location.search).toBe("?role=admin"));
    await waitFor(() =>
      expect(within(screen.getByRole("table")).getAllByRole("row")).toHaveLength(2),
    );
  });

  it("admins disable users with a reason, but can't touch admins or change roles", async () => {
    const { user, db } = renderAt("/users");
    await screen.findByRole("table");
    expect(screen.queryByRole("combobox", { name: /Role of/ })).not.toBeInTheDocument();
    expect(within(row(/olive/)).getByText("Owners manage admins")).toBeInTheDocument();
    await user.click(within(row(/@sam/)).getByRole("button", { name: "Disable sam…" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Disable sam?" });
    await user.type(within(dialog).getByLabelText(/Reason/), "Spam");
    await user.click(within(dialog).getByRole("button", { name: "Disable" }));
    expect(
      await screen.findByText("sam is disabled and signed out everywhere."),
    ).toBeInTheDocument();
    expect(db.users.find((u) => u.id === IDS.user)?.disabled_reason).toBe("Spam");
    await user.click(within(row(/@gone/)).getByRole("button", { name: "Enable gone" }));
    await user.click(
      within(await screen.findByRole("dialog", { name: "Enable gone?" })).getByRole("button", {
        name: "Enable",
      }),
    );
    expect(await screen.findByText("gone is enabled.")).toBeInTheDocument();
  });

  it("owners change roles after confirmation", async () => {
    const { user, db } = renderAt("/users", "owner");
    await screen.findByRole("table");
    await user.selectOptions(screen.getByRole("combobox", { name: "Role of sam" }), "admin");
    const dialog = await screen.findByRole("dialog", { name: "Make sam admin?" });
    await user.click(within(dialog).getByRole("button", { name: "Change role" }));
    expect(await screen.findByText("sam is now admin.")).toBeInTheDocument();
    expect(db.users.find((u) => u.id === IDS.user)?.role).toBe("admin");
  });

  it("explains disabling yourself and removing the last owner", async () => {
    const { user } = renderAt("/users", "owner");
    await screen.findByRole("table");
    await user.click(within(row(/@olive/)).getByRole("button", { name: "Disable olive…" }));
    await user.click(
      within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Disable" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "You can't disable your own account.",
    );
    await user.selectOptions(screen.getByRole("combobox", { name: "Role of olive" }), "admin");
    await user.click(
      within(await screen.findByRole("dialog")).getByRole("button", { name: "Change role" }),
    );
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        "The server needs at least one active owner.",
      ),
    );
  });

  it("handles a 403 even when the control was shown", async () => {
    const { user } = renderAt("/users", "owner", {
      overrides: [
        http.patch("*/v1/admin/users/:id", () => problem(403, "forbidden", "Not allowed")),
      ],
    });
    await screen.findByRole("table");
    await user.selectOptions(screen.getByRole("combobox", { name: "Role of sam" }), "admin");
    await user.click(
      within(await screen.findByRole("dialog")).getByRole("button", { name: "Change role" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent("Your role doesn't allow this.");
  });
});

describe("allowlist", () => {
  it("validates, adds, refuses duplicates and removes", async () => {
    const { user, db } = renderAt("/allowlist");
    await screen.findByRole("table");
    const id = screen.getByLabelText("Discord id");
    await user.type(id, "12ab");
    await user.click(screen.getByRole("button", { name: "Add" }));
    expect(screen.getByText(/A Discord id is 5 to 25 digits/)).toBeInTheDocument();
    expect(id).toHaveAttribute("aria-invalid", "true");
    await user.clear(id);
    await user.type(id, "300000000000000001");
    await user.type(screen.getByLabelText("Note (optional)"), "Nico");
    await user.click(screen.getByRole("button", { name: "Add" }));
    expect(await screen.findByText("300000000000000001 can now sign in.")).toBeInTheDocument();
    expect(db.allowlist[0]).toMatchObject({ discord_id: "300000000000000001", note: "Nico" });
    await user.type(id, "200000000000000001");
    await user.click(screen.getByRole("button", { name: "Add" }));
    expect(
      await screen.findByText("This Discord id is already on the allowlist."),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Remove 200000000000000002…" }));
    await user.click(
      within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Remove" }),
    );
    expect(await screen.findByText("200000000000000002 was removed.")).toBeInTheDocument();
    expect(db.allowlist.some((e) => e.discord_id === "200000000000000002")).toBe(false);
  });
});

describe("settings", () => {
  it("is read-only for admins", async () => {
    renderAt("/settings");
    expect(await screen.findByText("Only an owner can change these settings.")).toBeInTheDocument();
    expect(screen.getByLabelText("Server name (required)")).toHaveAttribute("readonly");
    expect(screen.queryByRole("button", { name: "Save settings" })).not.toBeInTheDocument();
  });

  it("owners save with If-Match, and a 412 keeps their input", async () => {
    const { user, db } = renderAt("/settings", "owner");
    const name = await screen.findByLabelText("Server name (required)");
    await user.clear(name);
    await user.type(name, "Saturday Games");
    await user.click(screen.getByRole("radio", { name: /Closed/ }));
    await user.click(screen.getByRole("button", { name: "Save settings" }));
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
    expect(db.settings).toMatchObject({ name: "Saturday Games", registration_mode: "closed" });

    // Someone else saves meanwhile.
    db.settings = { ...db.settings, motd: "Theirs" };
    db.settingsEtag += 1;
    await user.type(name, "!");
    await user.click(screen.getByRole("button", { name: "Save settings" }));
    expect(await screen.findByText("Changed by someone else")).toBeInTheDocument();
    expect(name).toHaveValue("Saturday Games!");
    await user.click(screen.getByRole("button", { name: "Keep my changes (then save)" }));
    await user.click(screen.getByRole("button", { name: "Save settings" }));
    await waitFor(() => expect(db.settings.name).toBe("Saturday Games!"));
    expect(db.settings.motd).toBe("Theirs");
  });

  it("checks the name length", async () => {
    const { user } = renderAt("/settings", "owner");
    const name = await screen.findByLabelText("Server name (required)");
    await user.clear(name);
    await user.click(screen.getByRole("button", { name: "Save settings" }));
    expect(screen.getByText("1 to 100 characters.")).toBeInTheDocument();
  });
});

describe("trust", () => {
  it("warns about keys expiring within 60 days and marks revoked ones", async () => {
    renderAt("/trust");
    const table = await screen.findByRole("table", { name: "Publisher keys" });
    expect(within(table).getByRole("row", { name: /Olive's desktop/ })).toHaveTextContent(
      /Expires in (19|20) days/,
    );
    expect(within(table).getByRole("row", { name: /Old build server/ })).toHaveTextContent(
      "Revoked",
    );
    expect(within(table).getByRole("row", { name: /Adrian's laptop/ })).toHaveTextContent("Valid");
    expect(screen.getByText("Only an owner can upload a new trust bundle.")).toBeInTheDocument();
  });

  it("owners upload a bundle; a bad signature and an old version are explained", async () => {
    const { user, db } = renderAt("/trust", "owner");
    const form = await screen.findByRole("form", { name: "Upload a trust bundle" });
    const bundle = (v: number) =>
      new File([JSON.stringify({ format: "vgames.trust/1", version: v })], "bundle.json");
    await user.upload(within(form).getByLabelText("Bundle (.json)"), bundle(8));
    await user.upload(
      within(form).getByLabelText("Signature (.sig)"),
      new File([btoa("forged")], "bundle.sig"),
    );
    await user.click(within(form).getByRole("button", { name: "Upload bundle" }));
    expect(await within(form).findByRole("alert")).toHaveTextContent(
      "isn't from this server's root key",
    );
    await user.upload(
      within(form).getByLabelText("Signature (.sig)"),
      new File([btoa("root-signature")], "bundle.sig"),
    );
    await user.click(within(form).getByRole("button", { name: "Upload bundle" }));
    expect(await screen.findByText("Trust bundle version 8 is active.")).toBeInTheDocument();
    expect(db.trust.version).toBe(8);
    await user.click(within(form).getByRole("button", { name: "Upload bundle" }));
    expect(await within(form).findByRole("alert")).toHaveTextContent(
      "already has this bundle version",
    );
  });
});

describe("jobs", () => {
  it("filters by state and retries dead jobs", async () => {
    const { user, db } = renderAt("/jobs?state=dead");
    const table = await screen.findByRole("table", { name: "Jobs" });
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows.length).toBeGreaterThan(0);
    for (const r of rows) expect(r).toHaveTextContent("dead");
    // Server text is shown as text.
    expect(
      within(table).getAllByText("IGDB answered 503 <Service Unavailable>").length,
    ).toBeGreaterThan(0);
    const queuedBefore = db.jobs.filter((j) => j.state === "queued").length;
    await user.click(within(rows[0] as HTMLElement).getByRole("button", { name: /^Retry/ }));
    expect(await screen.findByText(/is queued again/)).toBeInTheDocument();
    expect(db.jobs.filter((j) => j.state === "queued").length).toBe(queuedBefore + 1);
  });

  it("says when a job is already queued (409)", async () => {
    const { user } = renderAt("/jobs?state=dead", "admin", {
      overrides: [
        http.post("*/v1/admin/jobs/:id/retry", () =>
          problem(409, "job_already_queued", "Already queued"),
        ),
      ],
    });
    const table = await screen.findByRole("table", { name: "Jobs" });
    await user.click(within(table).getAllByRole("button", { name: /^Retry/ })[0] as HTMLElement);
    expect(await screen.findByRole("alert")).toHaveTextContent("already queued or running");
  });
});

describe("audit log", () => {
  it("pages with the cursor, filters through the URL and shows details as text", async () => {
    const { user, router } = renderAt("/audit");
    const table = await screen.findByRole("table", { name: "Audit entries, newest first" });
    expect(within(table).getAllByRole("row")).toHaveLength(51);
    await user.click(screen.getByRole("button", { name: "Load more" }));
    await waitFor(() => expect(within(table).getAllByRole("row")).toHaveLength(101));
    const details = within(table).getAllByText("Details")[0];
    if (!details) throw new Error("no details");
    await user.click(details);
    expect(within(table).getAllByText(/"note": "<b>entry 0<\/b>"/)[0]).toBeInTheDocument();
    expect(table.querySelector("b")).toBeNull();

    await user.type(screen.getByLabelText("Action"), "user.disable");
    await user.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(router.state.location.search).toBe("?action=user.disable"));
    await waitFor(() => {
      const rows = within(screen.getByRole("table")).getAllByRole("row").slice(1);
      expect(rows.length).toBe(26);
    });
  });

  it("checks the actor id", async () => {
    const { user } = renderAt("/audit");
    await screen.findByRole("table");
    await user.type(screen.getByLabelText("Actor (user id)"), "adrian");
    await user.click(screen.getByRole("button", { name: "Apply" }));
    expect(screen.getByRole("alert")).toHaveTextContent("a UUID");
  });
});
