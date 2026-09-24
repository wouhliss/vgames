// Routes. Every screen is its own chunk (route-level code splitting); the dev gallery exists only
// in development builds.
import { createBrowserRouter, type RouteObject } from "react-router";

const devRoutes: RouteObject[] = import.meta.env.DEV
  ? [
      {
        path: "/dev/gallery",
        lazy: async () => ({ Component: (await import("../routes/dev/Gallery")).Gallery }),
      },
    ]
  : [];

export function createAppRouter() {
  return createBrowserRouter([
    ...devRoutes,
    {
      path: "*",
      lazy: async () => ({ Component: (await import("../routes/Placeholder")).Placeholder }),
    },
  ]);
}
