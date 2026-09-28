import { createBrowserRouter, Navigate, type RouteObject } from "react-router";
import { LoginPage } from "../pages/LoginPage";
import { Placeholder } from "../pages/Placeholder";
import { ImagesTab } from "../pages/packages/ImagesTab";
import { MetadataTab } from "../pages/packages/MetadataTab";
import { PackageCreate } from "../pages/packages/PackageCreate";
import { PackageEditor } from "../pages/packages/PackageEditor";
import { PackageLayout } from "../pages/packages/PackageLayout";
import { PackagesList } from "../pages/packages/PackagesList";
import { UploadWizard } from "../pages/packages/UploadWizard";
import { VersionsTab } from "../pages/packages/VersionsTab";
import { NotFoundPage } from "./ErrorView";
import { Layout } from "./Layout";

export const routes: RouteObject[] = [
  { path: "/login", Component: LoginPage },
  {
    path: "/",
    Component: Layout,
    children: [
      { index: true, element: <Navigate to="/packages" replace /> },
      { path: "packages", Component: PackagesList },
      { path: "packages/new", Component: PackageCreate },
      {
        path: "packages/:packageId",
        Component: PackageLayout,
        children: [
          { index: true, Component: PackageEditor },
          { path: "metadata", Component: MetadataTab },
          { path: "images", Component: ImagesTab },
          { path: "versions", Component: VersionsTab },
          { path: "versions/new", Component: UploadWizard },
          { path: "versions/:versionId/upload", Component: UploadWizard },
        ],
      },
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
