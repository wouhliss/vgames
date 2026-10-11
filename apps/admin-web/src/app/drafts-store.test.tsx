import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { expect, it } from "vitest";
import { z } from "zod";
import { isSigningOut, signedInAgain, stashDrafts, useDraft } from "./drafts";
import { meKey } from "./session";

const Schema = z.object({ text: z.string(), etag: z.string() });
const draft = { text: "Unsaved input", etag: '"revision-one"' };
function form(user: string, snapshot: () => typeof draft | null = () => draft) {
  const queryClient = new QueryClient();
  queryClient.setQueryData(meKey, { user: { id: user } });
  return renderHook(() => useDraft("example", Schema, snapshot), {
    wrapper: ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    ),
  });
}

it("restores input and its original version once to the same account", () => {
  const first = form("account-one");
  act(() => stashDrafts());
  expect(isSigningOut()).toBe(true);
  first.unmount();
  signedInAgain();
  const again = form("account-one");
  expect(again.result.current).toEqual(draft);
  again.unmount();
  expect(form("account-one").result.current).toBeNull();
});

it("does not reveal a draft to another account", () => {
  const first = form("account-one");
  act(() => stashDrafts());
  first.unmount();
  expect(form("account-two").result.current).toBeNull();
  expect(sessionStorage.getItem("vgames.admin.draft:example")).toBeNull();
});

it("rejects expired, future-dated and malformed drafts", () => {
  for (const stored of [
    { user: "account-one", at: Date.now() - 25 * 3600000, value: draft },
    { user: "account-one", at: Date.now() + 3600000, value: draft },
    { user: "account-one", at: Date.now(), value: { text: 42 } },
  ]) {
    sessionStorage.setItem("vgames.admin.draft:example", JSON.stringify(stored));
    const current = form("account-one");
    expect(current.result.current).toBeNull();
    current.unmount();
  }
});

it("an untouched form stores nothing", () => {
  form("account-one", () => null);
  act(() => stashDrafts());
  expect(sessionStorage.getItem("vgames.admin.draft:example")).toBeNull();
});
