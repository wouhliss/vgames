import { useQueryClient } from "@tanstack/react-query";
import { Navigate, NavLink, Outlet, useLocation } from "react-router";
import { ApiError } from "../api/errors";
import { useOffline } from "./connectivity";
import { Loading, QueryState } from "./ErrorView";
import { currentReturnTo, useMe } from "./session";

const NAV = [
  { to: "/packages", label: "Packages" },
  { to: "/users", label: "Users" },
  { to: "/allowlist", label: "Allowlist" },
  { to: "/settings", label: "Settings" },
  { to: "/trust", label: "Trust" },
  { to: "/jobs", label: "Jobs" },
  { to: "/audit", label: "Audit log" },
];

export function OfflineBanner() {
  const offline = useOffline();
  const client = useQueryClient();
  if (!offline) return null;
  return (
    <div className="banner warn" role="status">
      <strong>Can't reach the server.</strong> Changes you haven't saved are kept on this page.{" "}
      <button
        type="button"
        onClick={() => {
          void client.refetchQueries({ type: "active" });
          void client.resumePausedMutations();
        }}
      >
        Retry now
      </button>
    </div>
  );
}

export function Layout() {
  const me = useMe();
  const location = useLocation();
  if (me.isPending) return <Loading label="Checking your session…" />;
  if (me.error) {
    if (me.error instanceof ApiError && me.error.status === 401) {
      const returnTo = currentReturnTo(`/admin${location.pathname}`);
      return <Navigate to={`/login?return_to=${encodeURIComponent(returnTo)}`} replace />;
    }
    return (
      <main className="center">
        <QueryState error={me.error} onRetry={() => void me.refetch()} />
      </main>
    );
  }
  const user = me.data.user;
  if (user.role === "user") {
    return (
      <main className="center">
        <h1>Admins only</h1>
        <p>You're signed in as {user.username}, but this area is for server admins.</p>
      </main>
    );
  }
  return (
    <div className="layout">
      <nav className="sidenav" aria-label="Admin">
        <p className="brand">vgames admin</p>
        <ul>
          {NAV.map((item) => (
            <li key={item.to}>
              <NavLink to={item.to}>{item.label}</NavLink>
            </li>
          ))}
        </ul>
        <p className="who">
          {user.display_name ?? user.username} ({user.role})
        </p>
      </nav>
      <main id="main">
        <OfflineBanner />
        <Outlet />
      </main>
    </div>
  );
}
