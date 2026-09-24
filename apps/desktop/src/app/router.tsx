// Routes. Every screen is its own chunk (route-level code splitting); the dev gallery exists only
// in development builds.
import { createBrowserRouter, Navigate, type RouteObject } from "react-router";
import { RouteError } from "./ErrorBoundary";
import { RootLayout } from "./RootLayout";
import { Shell } from "./Shell";

const devRoutes: RouteObject[] = import.meta.env.DEV
  ? [
      {
        path: "dev/gallery",
        lazy: async () => ({ Component: (await import("../routes/dev/Gallery")).Gallery }),
      },
    ]
  : [];

export const routes: RouteObject[] = [
  {
    Component: RootLayout,
    errorElement: <RouteError />,
    children: [
      ...devRoutes,
      {
        path: "onboarding",
        lazy: async () => ({
          Component: (await import("../routes/onboarding/Onboarding")).Onboarding,
        }),
      },
      {
        path: "/",
        Component: Shell,
        errorElement: <RouteError />,
        children: [
          { index: true, element: <Navigate to="/library" replace /> },
          {
            path: "library",
            lazy: async () => ({
              Component: (await import("../routes/library/LibraryPage")).LibraryPage,
            }),
          },
          {
            path: "browse",
            lazy: async () => ({
              Component: (await import("../routes/browse/BrowsePage")).BrowsePage,
            }),
          },
          {
            path: "friends",
            lazy: async () => ({
              Component: (await import("../routes/friends/FriendsPage")).FriendsPage,
            }),
          },
          {
            path: "downloads",
            lazy: async () => ({
              Component: (await import("../routes/downloads/DownloadsPage")).DownloadsPage,
            }),
          },
          {
            path: "settings/*",
            lazy: async () => ({
              Component: (await import("../routes/settings/SettingsPage")).SettingsPage,
            }),
          },
        ],
      },
      { path: "*", element: <Navigate to="/library" replace /> },
    ],
  },
];

export function createAppRouter() {
  return createBrowserRouter(routes);
}
