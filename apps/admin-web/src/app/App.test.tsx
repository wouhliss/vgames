import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { HttpResponse, http, type RequestHandler } from "msw";
import { createMemoryRouter, RouterProvider } from "react-router";
import { describe, expect, it, vi } from "vitest";
import { createQueryClient } from "../api/query";
import { createDb, type Role } from "../mocks/db";
import { createHandlers, problem } from "../mocks/handlers";
import { navigation } from "../pages/LoginPage";
import { server } from "../test/setup";
import { routes } from "./router";

/** The test's overrides take precedence over the default handlers (first match wins). */
function renderAt(path: string, role: Role | null, ...overrides: RequestHandler[]) {
  const db = createDb(role);
  // biome-ignore lint/suspicious/noDocumentCookie: simulates the server-set CSRF cookie.
  document.cookie = `__Host-vgames_csrf=${db.csrf}; path=/; secure`;
  server.use(...overrides, ...createHandlers(db));
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  const qc = createQueryClient({ onNetworkError: () => {} });
  render(
    <QueryClientProvider client={qc}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { router, db };
}

describe("admin shell", () => {
  it("shows navigation for admins", async () => {
    renderAt("/packages", "admin");
    expect(await screen.findByRole("navigation", { name: "Admin" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Packages" })).toBeInTheDocument();
    expect(screen.getByText("Adrian (admin)")).toBeInTheDocument();
  });

  it("sends signed-out users to sign-in with return_to", async () => {
    const { router } = renderAt("/users", null);
    await waitFor(() => expect(router.state.location.pathname).toBe("/login"));
    expect(router.state.location.search).toBe(`?return_to=${encodeURIComponent("/admin/users")}`);
  });

  it("refuses plain users", async () => {
    renderAt("/packages", "user");
    expect(await screen.findByRole("heading", { name: "Admins only" })).toBeInTheDocument();
  });

  it("shows a server error with retry when /v1/me fails", async () => {
    const { router } = renderAt(
      "/packages",
      "admin",
      http.get("*/v1/me", () => problem(500, "internal", "Internal error")),
    );
    expect(await screen.findByRole("alert", {}, { timeout: 8000 })).toHaveTextContent(
      "Server error",
    );
    expect(router.state.location.pathname).toBe("/packages");
  }, 15_000);
});

const authorizeOk = (bodies: unknown[]) =>
  http.post("*/v1/auth/discord/start", async ({ request }) => {
    bodies.push(await request.json());
    return HttpResponse.json({
      authorize_url: "https://discord.example/auth",
      expires_at: "2026-09-24T10:10:00Z",
    });
  });

describe("login", () => {
  it("starts Discord sign-in with return_to and follows the authorize URL", async () => {
    const assign = vi.spyOn(navigation, "assign").mockImplementation(() => {});
    const bodies: unknown[] = [];
    renderAt(`/login?return_to=${encodeURIComponent("/admin/users")}`, null, authorizeOk(bodies));
    await userEvent
      .setup()
      .click(await screen.findByRole("button", { name: "Sign in with Discord" }));
    await waitFor(() => expect(assign).toHaveBeenCalledWith("https://discord.example/auth"));
    expect(bodies).toEqual([{ client: "web", return_to: "/admin/users" }]);
  });

  it("ignores a return_to outside the admin app", async () => {
    vi.spyOn(navigation, "assign").mockImplementation(() => {});
    const bodies: unknown[] = [];
    renderAt(
      `/login?return_to=${encodeURIComponent("https://evil.example/")}`,
      null,
      authorizeOk(bodies),
    );
    await userEvent
      .setup()
      .click(await screen.findByRole("button", { name: "Sign in with Discord" }));
    await waitFor(() => expect(bodies).toEqual([{ client: "web", return_to: "/admin/" }]));
  });

  it("explains rate limiting with a countdown", async () => {
    renderAt(
      "/login",
      null,
      http.post("*/v1/auth/discord/start", () =>
        HttpResponse.json(
          {
            type: "urn:vgames:problem:rate_limited",
            title: "Too many",
            status: 429,
            code: "rate_limited",
          },
          {
            status: 429,
            headers: { "Retry-After": "30", "Content-Type": "application/problem+json" },
          },
        ),
      ),
    );
    await userEvent
      .setup()
      .click(await screen.findByRole("button", { name: "Sign in with Discord" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Too many requests");
    expect(screen.getByRole("button", { name: /Retry in \d+ s/ })).toBeDisabled();
  });

  it("shows callback errors", async () => {
    renderAt("/login?error=not_allowlisted", null);
    expect(await screen.findByRole("alert")).toHaveTextContent("isn't on this server's allowlist");
  });
});
