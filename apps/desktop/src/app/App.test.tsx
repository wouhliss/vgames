import type { UnlistenFn } from "@tauri-apps/api/event";
import { act, render, screen, waitFor } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { events } from "../ipc";
import { installMockBackend, OTHER_FINGERPRINT } from "../mocks/backend";
import { flush } from "../test/render";
import { App } from "./App";
import { routes } from "./router";

function deferred<T>() {
  let resolve: (value: T) => void = () => {
    throw new Error("Deferred promise has not been initialized.");
  };
  const promise = new Promise<T>((finish) => {
    resolve = finish;
  });
  return { promise, resolve };
}

beforeEach(() => {
  window.history.replaceState(null, "", "/onboarding");
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
  // An initially hidden WebKitGTK window can stop delivering animation frames.
  vi.stubGlobal(
    "requestAnimationFrame",
    vi.fn(() => 1),
  );
  vi.stubGlobal("cancelAnimationFrame", vi.fn());
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  window.history.replaceState(null, "", "/");
});

it("signals native readiness without animation frames after the first route commits", async () => {
  const backend = installMockBackend();
  render(
    <StrictMode>
      <App />
    </StrictMode>,
  );

  expect(await screen.findByRole("heading", { name: "Welcome to vgames" })).toBeInTheDocument();
  await waitFor(() => expect(backend.callsTo("app_ready")).toHaveLength(1));
});

it("keeps the window hidden while the initial lazy route is still loading", async () => {
  const backend = installMockBackend();
  const loaded = deferred<void>();
  const route = routes[0]?.children?.find((entry) => entry.path === "onboarding");
  if (!route || typeof route.lazy !== "function") throw new Error("Missing onboarding lazy route.");
  const originalLoader = route.lazy;
  const loader = vi
    .spyOn(route as { lazy: typeof originalLoader }, "lazy")
    .mockImplementation(async () => {
      await loaded.promise;
      return originalLoader();
    });
  render(<App />);

  await waitFor(() => expect(loader).toHaveBeenCalled());
  await flush();
  expect(screen.queryByRole("heading", { name: "Welcome to vgames" })).not.toBeInTheDocument();
  expect(backend.callsTo("app_ready")).toHaveLength(0);

  await act(async () => loaded.resolve());
  expect(await screen.findByRole("heading", { name: "Welcome to vgames" })).toBeInTheDocument();
  await waitFor(() => expect(backend.callsTo("app_ready")).toHaveLength(1));
});

it("waits for the startup deep-link listener acknowledgement before releasing readiness", async () => {
  const backend = installMockBackend();
  const acknowledged = deferred<UnlistenFn>();
  const unlisten = vi.fn();
  const listen = vi
    .spyOn(events.serverAddRequested, "listen")
    .mockReturnValue(acknowledged.promise);
  const view = render(<App />);

  expect(await screen.findByRole("heading", { name: "Welcome to vgames" })).toBeInTheDocument();
  expect(listen).toHaveBeenCalledTimes(1);
  expect(backend.callsTo("app_ready")).toHaveLength(0);

  await act(async () => acknowledged.resolve(unlisten));
  await waitFor(() => expect(backend.callsTo("app_ready")).toHaveLength(1));
  view.unmount();
  expect(unlisten).toHaveBeenCalledTimes(1);
});

it("cleans a late subscription without signalling readiness after unmount", async () => {
  const backend = installMockBackend();
  const acknowledged = deferred<UnlistenFn>();
  const unlisten = vi.fn();
  const listen = vi
    .spyOn(events.serverAddRequested, "listen")
    .mockReturnValue(acknowledged.promise);
  const view = render(<App />);

  await waitFor(() => expect(listen).toHaveBeenCalledTimes(1));
  view.unmount();
  await act(async () => acknowledged.resolve(unlisten));
  await flush();

  expect(unlisten).toHaveBeenCalledTimes(1);
  expect(backend.callsTo("app_ready")).toHaveLength(0);
});

it("uses only the active StrictMode subscription to signal readiness", async () => {
  const backend = installMockBackend();
  const stale = deferred<UnlistenFn>();
  const active = deferred<UnlistenFn>();
  const stopStale = vi.fn();
  const stopActive = vi.fn();
  const listen = vi
    .spyOn(events.serverAddRequested, "listen")
    .mockReturnValueOnce(stale.promise)
    .mockReturnValueOnce(active.promise);
  const view = render(
    <StrictMode>
      <App />
    </StrictMode>,
  );

  await waitFor(() => expect(listen).toHaveBeenCalledTimes(2));
  await act(async () => stale.resolve(stopStale));
  expect(stopStale).toHaveBeenCalledTimes(1);
  expect(backend.callsTo("app_ready")).toHaveLength(0);

  await act(async () => active.resolve(stopActive));
  await waitFor(() => expect(backend.callsTo("app_ready")).toHaveLength(1));
  view.unmount();
  expect(stopActive).toHaveBeenCalledTimes(1);
});

it("consumes a pinned startup server link emitted immediately when native readiness is released", async () => {
  const backend = installMockBackend();
  const warnings = vi.spyOn(console, "warn");
  const link = { url: "https://startup.example/", fingerprint: OTHER_FINGERPRINT };
  let released = false;
  backend.on("app_ready", async () => {
    if (!released) {
      released = true;
      await events.serverAddRequested.emit(link);
    }
    return null;
  });
  render(
    <StrictMode>
      <App />
    </StrictMode>,
  );

  await waitFor(() =>
    expect(backend.callsTo("server_preview")[0]?.args).toEqual({
      url: link.url,
      expectedFingerprint: link.fingerprint,
    }),
  );
  expect(await screen.findByText(link.fingerprint)).toBeInTheDocument();
  expect(backend.callsTo("app_ready")).toHaveLength(1);
  expect(warnings).not.toHaveBeenCalled();
});
