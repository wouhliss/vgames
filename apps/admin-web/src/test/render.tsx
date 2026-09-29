import { QueryClientProvider } from "@tanstack/react-query";
import { render } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { RequestHandler } from "msw";
import type { ReactNode } from "react";
import { createMemoryRouter, RouterProvider } from "react-router";
import { setUnauthorizedHandler } from "../api/http";
import { createQueryClient } from "../api/query";
import { routes } from "../app/router";
import { leaveForSignIn } from "../app/session";
import { createDb, type MockDb, type Role } from "../mocks/db";
import { createHandlers } from "../mocks/handlers";
import { server } from "./setup";

/** Renders the admin app at `path` against the MSW handlers; `overrides` win (first match). */
export function renderAt(
  path: string,
  role: Role | null = "admin",
  options: {
    overrides?: RequestHandler[];
    db?: (db: MockDb) => void;
    wrapper?: (props: { children: ReactNode }) => ReactNode;
    /** Continue with this mock server state (a reload in the middle of a test). */
    existing?: MockDb;
  } = {},
) {
  const db = options.existing ?? createDb(role);
  options.db?.(db);
  // biome-ignore lint/suspicious/noDocumentCookie: simulates the server-set CSRF cookie.
  document.cookie = `__Host-vgames_csrf=${db.csrf}; path=/; secure`;
  server.use(...(options.overrides ?? []), ...createHandlers(db));
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  // A 401 behaves as in the app: drafts are kept and the router goes to sign-in.
  const qc = createQueryClient({ onNetworkError: () => {} });
  setUnauthorizedHandler(() =>
    leaveForSignIn(
      qc,
      (to) => void router.navigate(to),
      `/admin${router.state.location.pathname}`,
      router.state.location.pathname.startsWith("/login"),
    ),
  );
  const Wrapper = options.wrapper ?? (({ children }: { children: ReactNode }) => children);
  render(
    <QueryClientProvider client={qc}>
      <Wrapper>
        <RouterProvider router={router} />
      </Wrapper>
    </QueryClientProvider>,
  );
  return { router, db, qc, user: userEvent.setup() };
}
