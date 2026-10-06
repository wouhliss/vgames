import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { delay, HttpResponse, http } from "msw";
import { describe, expect, it, vi } from "vitest";
import { createQueryClient } from "../../api/query";
import type { SignedCompatProfile } from "../../api/schemas";
import { problem } from "../../mocks/handlers";
import { server } from "../../test/setup";
import { CompatHistory } from "./CompatHistory";

const HISTORY_URL = "*/v1/admin/packages/:id/compat/:target";
function item(revision: number): SignedCompatProfile {
  return {
    target: "linux",
    revision,
    status: "verified",
    document: "e30=",
    signature: {
      format: "vgames.sig/1",
      alg: "ed25519",
      context: "vgames/compat/v1",
      key_id: "a".repeat(32),
      payload_blake3: "b".repeat(64),
      signature: "AA==",
    },
    created_at: "2026-10-06T10:00:00Z",
  };
}
function show() {
  const signedOut = vi.fn();
  const client = createQueryClient({ onUnauthenticated: signedOut, onNetworkError: () => {} });
  render(
    <QueryClientProvider client={client}>
      <CompatHistory packageId="test-package" target="linux" />
    </QueryClientProvider>,
  );
  return { signedOut, user: userEvent.setup() };
}

describe("signed compatibility history", () => {
  it("shows loading, then an empty history", async () => {
    server.use(
      http.get(HISTORY_URL, async () => {
        await delay(50);
        return HttpResponse.json({ items: [] });
      }),
    );
    show();
    expect(screen.getByRole("status")).toHaveTextContent("Loading revision history");
    expect(await screen.findByText("No revisions yet.")).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("shows signer and time and follows the opaque cursor without losing earlier pages", async () => {
    const cursors: (string | null)[] = [];
    server.use(
      http.get(HISTORY_URL, ({ request }) => {
        const cursor = new URL(request.url).searchParams.get("cursor");
        cursors.push(cursor);
        return HttpResponse.json(
          cursor
            ? { items: [item(1)] }
            : { items: [item(3), item(2)], next_cursor: "signed-cursor" },
        );
      }),
    );
    const { user } = show();
    expect(await screen.findByText(/Revision 3:/)).toBeInTheDocument();
    expect(screen.getAllByText("a".repeat(32))).toHaveLength(2);
    expect(screen.getAllByText((_, el) => el?.tagName === "TIME")[0]).toHaveAttribute(
      "datetime",
      "2026-10-06T10:00:00Z",
    );
    await user.click(screen.getByRole("button", { name: "Load earlier revisions" }));
    expect(await screen.findByText(/Revision 1:/)).toBeInTheDocument();
    expect(screen.getAllByRole("listitem")).toHaveLength(3);
    expect(cursors).toEqual([null, "signed-cursor"]);
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it.each([
    [401, "Signed out"],
    [403, "Not allowed"],
    [400, "Invalid cursor"],
  ])("handles %s explicitly", async (status, title) => {
    server.use(http.get(HISTORY_URL, () => problem(status, "synthetic", title)));
    const { signedOut } = show();
    expect(await screen.findByRole("alert")).toHaveTextContent(title);
    if (status === 400) expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
    else expect(screen.queryByRole("button")).not.toBeInTheDocument();
    if (status === 401) await waitFor(() => expect(signedOut).toHaveBeenCalledOnce());
  });

  it("rejects malformed revisions at the response boundary and offers retry", async () => {
    server.use(
      http.get(HISTORY_URL, () => HttpResponse.json({ items: [{ ...item(1), revision: 0 }] })),
    );
    const { user } = show();
    expect(await screen.findByRole("alert")).toHaveTextContent("Unexpected response");
    expect(screen.queryByRole("listitem")).not.toBeInTheDocument();
    server.use(http.get(HISTORY_URL, () => HttpResponse.json({ items: [item(1)] })));
    await user.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText(/Revision 1:/)).toBeInTheDocument();
  });
});
