import { QueryClient } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";
import { readCsrfToken, setCsrfFallback } from "../api/http";
import { leaveForSignIn, meKey } from "./session";

describe("session expiry", () => {
  it("clears previous account data and cancels in-flight reads before sign-in", async () => {
    const client = new QueryClient();
    client.setQueryData(meKey, { user: { id: "previous-admin" } });
    client.setQueryData(["users"], [{ display_name: "Previous account data" }]);
    setCsrfFallback("synthetic-previous-csrf");
    let requestSignal: AbortSignal | undefined;
    const pending = client
      .fetchQuery({
        queryKey: ["packages"],
        queryFn: ({ signal }) => {
          requestSignal = signal;
          return new Promise<never>((_, reject) => {
            signal.addEventListener("abort", () => reject(new Error("Cancelled")), { once: true });
          });
        },
      })
      .catch(() => undefined);
    const navigate = vi.fn();
    try {
      leaveForSignIn(client, navigate, "/admin/packages", false);
      expect(requestSignal?.aborted).toBe(true);
      expect(client.getQueryCache().getAll()).toHaveLength(0);
      expect(readCsrfToken()).toBeNull();
      expect(navigate).toHaveBeenCalledWith("/login?return_to=%2Fadmin%2Fpackages");
    } finally {
      client.clear();
      await pending;
    }
  });
});
