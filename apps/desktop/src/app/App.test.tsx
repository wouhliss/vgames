import { render, screen } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { installMockBackend } from "../mocks/backend";
import { App } from "./App";

afterEach(() => {
  vi.unstubAllGlobals();
  window.history.replaceState(null, "", "/");
});

it("signals native readiness when the hidden WebView does not deliver animation frames", async () => {
  const backend = installMockBackend();
  window.history.replaceState(null, "", "/onboarding");
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
  // WebKitGTK does not paint the initially hidden window until app_ready shows it.
  vi.stubGlobal(
    "requestAnimationFrame",
    vi.fn(() => 1),
  );
  vi.stubGlobal("cancelAnimationFrame", vi.fn());

  render(
    <StrictMode>
      <App />
    </StrictMode>,
  );

  expect(await screen.findByRole("heading", { name: "Welcome to vgames" })).toBeInTheDocument();
  expect(backend.callsTo("app_ready")).not.toHaveLength(0);
});
