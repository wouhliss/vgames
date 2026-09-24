import { createBrowserRouter, Navigate, type RouteObject } from "react-router";
import { LoginPage } from "../pages/LoginPage";
import { Placeholder } from "../pages/Placeholder";
import { NotFoundPage } from "./ErrorView";
import { Layout } from "./Layout";

export const routes: RouteObject[] = [
  { path: "/login", Component: LoginPage },
  {
    path: "/",
    Component: Layout,
    children: [
      { index: true, element: <Navigate to="/packages" replace /> },
      { path: "packages/*", element: <Placeholder title="Packages" /> },
      { path: "users", element: <Placeholder title="Users" /> },
      { path: "allowlist", element: <Placeholder title="Allowlist" /> },
      { path: "settings", element: <Placeholder title="Settings" /> },
      { path: "trust", element: <Placeholder title="Trust" /> },
      { path: "jobs", element: <Placeholder title="Jobs" /> },
      { path: "audit", element: <Placeholder title="Audit log" /> },
      { path: "*", Component: NotFoundPage },
    ],
  },
];

export function createAppRouter() {
  return createBrowserRouter(routes, { basename: "/admin" });
}
