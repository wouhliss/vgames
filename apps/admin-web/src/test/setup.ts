import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { setupServer } from "msw/node";
import { afterAll, afterEach, beforeAll } from "vitest";
import { setUnauthorizedHandler } from "../api/http";
import { resetDrafts } from "../app/drafts";

/** Tests add handlers per case with `server.use(...)`; unhandled requests fail the test. */
export const server = setupServer();

beforeAll(() => server.listen({ onUnhandledRequest: "error" }));
afterEach(() => {
  cleanup();
  server.resetHandlers();
  resetDrafts();
  setUnauthorizedHandler(null);
  // biome-ignore lint/suspicious/noDocumentCookie: simulates the server-set CSRF cookie.
  document.cookie = "__Host-vgames_csrf=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/";
});
afterAll(() => server.close());
