// One package: its header and the tabs (Details, Metadata, Images, …), each its own URL. The package
// and its ETag live in the query cache, shared by every tab; whichever tab writes puts the server's
// answer (and new ETag) back there.
import { useQuery } from "@tanstack/react-query";
import { Link, NavLink, Outlet, useOutletContext, useParams } from "react-router";
import type { ApiResponse } from "../../api/http";
import { getPackage, packageKeys } from "../../api/packages";
import type { AdminPackage } from "../../api/schemas";
import { Loading, QueryState } from "../../app/ErrorView";
import { STATUS_LABEL } from "./model";

export interface PackageContext {
  response: ApiResponse<AdminPackage>;
}

export function usePackageContext(): PackageContext {
  return useOutletContext<PackageContext>();
}

const TABS = [
  { to: "", label: "Details", end: true },
  { to: "metadata", label: "Metadata", end: false },
  { to: "images", label: "Images", end: false },
  { to: "versions", label: "Versions", end: false },
];

export function PackageLayout() {
  const id = useParams().packageId ?? "";
  const query = useQuery({
    queryKey: packageKeys.one(id),
    queryFn: ({ signal }) => getPackage(id, signal),
    // Forms own what is on screen; tabs refresh the cache when they write.
    staleTime: Number.POSITIVE_INFINITY,
  });
  if (query.isPending) return <Loading label="Loading package…" />;
  if (query.isError) return <QueryState error={query.error} onRetry={() => void query.refetch()} />;
  const pkg = query.data.data;
  return (
    <section aria-labelledby="page-title">
      <p>
        <Link to="/packages">← Packages</Link>
      </p>
      <div className="page-header">
        <h1 id="page-title" dir="auto">
          {pkg.title}
        </h1>
        <span className="badge">{STATUS_LABEL[pkg.status]}</span>
      </div>
      <p className="muted">
        <span className="mono">{pkg.id}</span> · created by{" "}
        {pkg.created_by.display_name ?? pkg.created_by.username}
      </p>
      <nav className="subnav" aria-label="Package sections">
        <ul>
          {TABS.map((tab) => (
            <li key={tab.label}>
              <NavLink to={tab.to ? `/packages/${id}/${tab.to}` : `/packages/${id}`} end={tab.end}>
                {tab.label}
              </NavLink>
            </li>
          ))}
        </ul>
      </nav>
      <Outlet context={{ response: query.data } satisfies PackageContext} />
    </section>
  );
}
