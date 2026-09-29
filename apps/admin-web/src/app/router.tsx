import { createBrowserRouter, Navigate, type RouteObject } from "react-router";
import { LoginPage } from "../pages/LoginPage";
import { CompatTab } from "../pages/packages/CompatTab";
import { ImagesTab } from "../pages/packages/ImagesTab";
import { MetadataTab } from "../pages/packages/MetadataTab";
import { PackageCreate } from "../pages/packages/PackageCreate";
import { PackageEditor } from "../pages/packages/PackageEditor";
import { PackageLayout } from "../pages/packages/PackageLayout";
import { PackagesList } from "../pages/packages/PackagesList";
import { UploadWizard } from "../pages/packages/UploadWizard";
import { VersionsTab } from "../pages/packages/VersionsTab";
import { AllowlistPage } from "../pages/server/AllowlistPage";
import { AuditPage } from "../pages/server/AuditPage";
import { JobsPage } from "../pages/server/JobsPage";
import { SettingsPage } from "../pages/server/SettingsPage";
import { TrustPage } from "../pages/server/TrustPage";
import { UsersPage } from "../pages/server/UsersPage";
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
          { path: "compatibility", Component: CompatTab },
          { path: "versions/new", Component: UploadWizard },
          { path: "versions/:versionId/upload", Component: UploadWizard },
        ],
      },
      { path: "users", Component: UsersPage },
      { path: "allowlist", Component: AllowlistPage },
      { path: "settings", Component: SettingsPage },
      { path: "trust", Component: TrustPage },
      { path: "jobs", Component: JobsPage },
      { path: "audit", Component: AuditPage },
      { path: "*", Component: NotFoundPage },
    ],
  },
];

export function createAppRouter() {
  return createBrowserRouter(routes, { basename: "/admin" });
}
