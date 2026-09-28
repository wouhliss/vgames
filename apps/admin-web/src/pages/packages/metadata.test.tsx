import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ApiError } from "../../api/errors";
import * as upload from "../../api/upload";
import { assetId, packageId } from "../../mocks/db";
import { editElsewhere } from "../../mocks/packages";
import { renderAt } from "../../test/render";
import { server } from "../../test/setup";

const HARBOR = packageId(1);
const CANYON = packageId(2);

function countRequests(pathPart: string) {
  const seen: string[] = [];
  server.events.on("request:start", ({ request }) => {
    if (request.url.includes(pathPart)) seen.push(request.method);
  });
  return seen;
}

const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0]);

describe("metadata review", () => {
  it("lists candidates and applies the ticked fields", async () => {
    const { user, db } = renderAt(`/packages/${HARBOR}/metadata`);
    await screen.findByText("Lookup finished.");
    const table = screen.getByRole("table", { name: "Metadata candidates, best first" });
    const rows = within(table).getAllByRole("row");
    expect(rows[1]).toHaveTextContent("IGDBHollow Harbor202497%");
    expect(rows[2]).toHaveTextContent("SteamHollow Harbor: Deluxe202481%");
    await user.click(screen.getByRole("radio", { name: "Compare Hollow Harbor (IGDB 1942)" }));
    const compare = await screen.findByRole("region", { name: /Compare with IGDB/ });
    // Genres were edited by an admin: shown, not ticked; differing fields are ticked.
    expect(within(compare).getByRole("checkbox", { name: "Apply Genres" })).not.toBeChecked();
    expect(within(compare).getByRole("checkbox", { name: "Apply Summary" })).toBeChecked();
    expect(within(compare).getByRole("checkbox", { name: "Apply Publisher" })).toBeChecked();
    // Same value → not ticked. A field the candidate lacks → can't be ticked.
    expect(within(compare).getByRole("checkbox", { name: "Apply Title" })).not.toBeChecked();
    expect(within(compare).getByRole("checkbox", { name: "Apply Logo" })).toBeDisabled();
    await user.click(within(compare).getByRole("checkbox", { name: "Apply Publisher" }));
    await user.click(within(compare).getByRole("button", { name: /^Apply \d+ fields?$/ }));
    expect(await screen.findByText(/^Applied \d+ fields/)).toBeInTheDocument();
    const saved = db.packages.find((p) => p.id === HARBOR);
    expect(saved?.summary).toBe("A fishing town hides an old secret beneath the tide.");
    expect(saved?.publisher).toBe("Tidewater Games");
    expect(saved?.genres).toEqual(["Adventure", "Simulation"]);
  });

  it("overwrites admin-edited fields only with the toggle, which warns", async () => {
    const { user, db } = renderAt(`/packages/${HARBOR}/metadata`);
    await user.click(await screen.findByRole("radio", { name: /Compare Hollow Harbor \(IGDB/ }));
    const compare = await screen.findByRole("region", { name: /Compare with IGDB/ });
    await user.click(within(compare).getByRole("checkbox", { name: "Apply Genres" }));
    expect(
      within(compare).getByText(/1 ticked field was edited by an admin and will be kept/),
    ).toBeInTheDocument();
    await user.click(
      within(compare).getByRole("checkbox", { name: "Overwrite fields an admin edited" }),
    );
    expect(within(compare).getByText(/typed by hand will be replaced/)).toBeInTheDocument();
    await user.click(within(compare).getByRole("button", { name: /^Apply \d+ fields?$/ }));
    await screen.findByText(/^Applied/);
    expect(db.packages.find((p) => p.id === HARBOR)?.genres).toEqual([
      "Adventure",
      "Simulation",
      "Indie",
    ]);
  });

  it("says when there are no candidates", async () => {
    renderAt(`/packages/${CANYON}/metadata`);
    expect(await screen.findByText(/No candidates found/)).toBeInTheDocument();
    expect(screen.getByText("No lookup has run for this package yet.")).toBeInTheDocument();
  });

  it("polls every 2 s while the job runs and stops at the end", async () => {
    const seen = countRequests("/metadata/candidates");
    const { user } = renderAt(`/packages/${CANYON}/metadata`);
    await user.click(await screen.findByRole("button", { name: "Look up again" }));
    expect(
      await screen.findByText(/Waiting to look up metadata|Looking up IGDB and Steam/),
    ).toBeInTheDocument();
    expect(await screen.findByText("Lookup finished.", {}, { timeout: 8000 })).toBeInTheDocument();
    expect((await screen.findAllByRole("radio", { name: /Compare Hollow Harbor/ })).length).toBe(2);
    const after = seen.length;
    await new Promise((r) => setTimeout(r, 2500));
    expect(seen.length).toBe(after);
  }, 15_000);

  it("shows a failed job with its error and retries it", async () => {
    const { user } = renderAt(`/packages/${CANYON}/metadata`, "admin", {
      db: (db) => {
        db.lookupOutcome[CANYON] = "fail";
      },
    });
    await user.click(await screen.findByRole("button", { name: "Look up again" }));
    expect(
      await screen.findByText(/The lookup failed and gave up/, {}, { timeout: 8000 }),
    ).toBeInTheDocument();
    expect(screen.getByText("IGDB answered 503 Service Unavailable")).toBeInTheDocument();
    expect(screen.getByText(/attempt 5 of 5/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText(/Waiting to look up|Looking up/)).toBeInTheDocument();
  }, 15_000);

  it("on 412 asks to reload before applying", async () => {
    const { user, db } = renderAt(`/packages/${HARBOR}/metadata`);
    await user.click(await screen.findByRole("radio", { name: /Compare Hollow Harbor \(IGDB/ }));
    const compare = await screen.findByRole("region", { name: /Compare with IGDB/ });
    editElsewhere(db, HARBOR, { summary: "Edited meanwhile" });
    await user.click(within(compare).getByRole("button", { name: /^Apply \d+ fields?$/ }));
    const alert = await within(compare).findByText("Changed by someone else");
    await user.click(
      within(alert.closest("[role=alert]") as HTMLElement).getByRole("button", {
        name: "Reload package",
      }),
    );
    await waitFor(() =>
      expect(within(compare).queryByText("Changed by someone else")).not.toBeInTheDocument(),
    );
    await waitFor(() =>
      expect(screen.getByRole("row", { name: /Summary/ })).toHaveTextContent("Edited meanwhile"),
    );
    await user.click(within(compare).getByRole("button", { name: /^Apply \d+ fields?$/ }));
    expect(await screen.findByText(/^Applied/)).toBeInTheDocument();
  });
});

describe("images", () => {
  it("shows current images and a placeholder for a broken one", async () => {
    renderAt(`/packages/${HARBOR}/images`);
    expect(await screen.findByRole("img", { name: "Cover of Hollow Harbor" })).toHaveAttribute(
      "src",
      `/v1/assets/${assetId(1)}`,
    );
    const broken = screen.getByRole("img", { name: "Screenshot 1 of Hollow Harbor" });
    broken.dispatchEvent(new Event("error"));
    expect(
      await screen.findByRole("img", {
        name: "Screenshot 1 of Hollow Harbor (couldn't be loaded)",
      }),
    ).toHaveTextContent("Image unavailable");
    expect(screen.getByText("No logo yet.")).toBeInTheDocument();
  });

  it("refuses a file that is too big or not an image, before uploading", async () => {
    const seen = countRequests("/assets");
    const { user } = renderAt(`/packages/${HARBOR}/images`);
    const input = await screen.findByLabelText("Upload a new logo");
    const big = new File([PNG, new Uint8Array(10 * 1024 * 1024)], "big.png", { type: "image/png" });
    await user.upload(input, big);
    expect(await screen.findByText(/images can be at most 10 MiB/)).toBeInTheDocument();
    const text = new File(["not an image"], "fake.png", { type: "image/png" });
    await user.upload(input, text);
    expect(
      await screen.findByText("Only JPEG, PNG and WebP images can be uploaded."),
    ).toBeInTheDocument();
    expect(input).toHaveAttribute("aria-invalid", "true");
    const logoSection = screen.getByRole("region", { name: "Logo" });
    expect(within(logoSection).getByRole("button", { name: "Upload" })).toBeDisabled();
    expect(seen.filter((m) => m === "POST")).toHaveLength(0);
  });

  // jsdom's File can't travel through MSW's XHR interceptor, so these stub the transport (tested on
  // its own in api/upload.test.ts, and end to end in the browser suite).
  it("uploads a logo and uses it", async () => {
    const { user, db } = renderAt(`/packages/${HARBOR}/images`);
    vi.spyOn(upload, "uploadAsset").mockImplementation(async (_pkg, kind, _file, onProgress) => {
      onProgress(0.5);
      const asset = {
        id: assetId(0x200),
        kind,
        url: `/v1/assets/${assetId(0x200)}`,
        width: 512,
        height: 512,
        content_type: "image/png" as const,
        source: "upload" as const,
      };
      db.uploadedAssets = { [asset.id]: asset };
      return asset;
    });
    const section = await screen.findByRole("region", { name: "Logo" });
    await user.upload(
      within(section).getByLabelText("Upload a new logo"),
      new File([PNG], "logo.png", { type: "image/png" }),
    );
    await user.click(within(section).getByRole("button", { name: "Upload" }));
    expect(await screen.findByText("New logo uploaded and in use.")).toBeInTheDocument();
    expect(db.packages.find((p) => p.id === HARBOR)?.logo?.source).toBe("upload");
    expect(
      await within(section).findByRole("img", { name: "Logo of Hollow Harbor" }),
    ).toBeInTheDocument();
    vi.restoreAllMocks();
  });

  it("shows the server's refusal of an upload", async () => {
    const { user } = renderAt(`/packages/${HARBOR}/images`);
    vi.spyOn(upload, "uploadAsset").mockRejectedValue(
      new ApiError({
        kind: "http",
        status: 415,
        problem: {
          type: "urn:vgames:problem:unsupported_media_type",
          title: "Only JPEG, PNG and WebP",
          status: 415,
          code: "unsupported_media_type",
        },
        retryAfterSeconds: null,
        requestId: null,
      }),
    );
    const section = await screen.findByRole("region", { name: "Screenshots" });
    await user.upload(
      within(section).getByLabelText("Upload a screenshot"),
      new File([PNG], "shot.png", { type: "image/png" }),
    );
    await user.click(within(section).getByRole("button", { name: "Upload" }));
    expect(await within(section).findByRole("alert")).toHaveTextContent("Only JPEG, PNG and WebP");
    vi.restoreAllMocks();
  });

  it("deletes an image after confirmation", async () => {
    const { user, db } = renderAt(`/packages/${HARBOR}/images`);
    await user.click(await screen.findByRole("button", { name: "Delete hero…" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Delete hero?" });
    expect(within(dialog).getByRole("button", { name: "Cancel" })).toHaveFocus();
    await user.click(within(dialog).getByRole("button", { name: "Delete" }));
    expect(await screen.findByText("Hero deleted.")).toBeInTheDocument();
    expect(db.packages.find((p) => p.id === HARBOR)?.hero).toBeUndefined();
    expect(await screen.findByText("No hero yet.")).toBeInTheDocument();
  });
});
