// The signed-in app: sidebar, top bar, banners and the routed content. Sends first-run users to
// onboarding until a server, an account and a library exist.
import { useState } from "react";
import { Navigate, NavLink, Outlet } from "react-router";
import { ErrorState, LoadingState } from "../components/Feedback";
import { Icon, type IconName } from "../components/Icon";
import { t } from "../i18n";
import { events } from "../ipc";
import { useTauriEvent } from "../ipc/events";
import { useActiveServer, useOnboardingNeed, useServers } from "./queries";
import styles from "./Shell.module.css";
import { AccountMenu, DownloadIndicator, ServerSwitcher, UpdateBannerSlot } from "./TopBar";

const NAV: { to: string; icon: IconName; label: () => string }[] = [
  { to: "/library", icon: "library", label: () => t("nav.library") },
  { to: "/browse", icon: "browse", label: () => t("nav.browse") },
  { to: "/friends", icon: "friends", label: () => t("nav.friends") },
  { to: "/downloads", icon: "download", label: () => t("nav.downloads") },
  { to: "/settings", icon: "settings", label: () => t("nav.settings") },
];

export function Shell() {
  const { need, loading, error } = useOnboardingNeed();
  const servers = useServers();
  const { server } = useActiveServer();
  const [offline, setOffline] = useState<Set<string>>(() => new Set());

  useTauriEvent(events.connectivityChanged, ({ server_id, online }) => {
    setOffline((prev) => {
      if (online === !prev.has(server_id)) return prev;
      const next = new Set(prev);
      if (online) next.delete(server_id);
      else next.add(server_id);
      return next;
    });
  });

  if (loading) {
    return (
      <div className={styles.gate}>
        <LoadingState />
      </div>
    );
  }
  if (error) {
    return (
      <div className={styles.gate}>
        <ErrorState title={t("error.generic")} onRetry={() => void servers.refetch()} />
      </div>
    );
  }
  if (need !== null || !server) return <Navigate to="/onboarding" replace />;

  return (
    <div className={styles.shell}>
      <a className={styles.skip} href="#main-content">
        {t("shell.skipToContent")}
      </a>
      <nav className={styles.sidebar} aria-label={t("nav.main")} data-nav-group="">
        <div className={styles.brand} aria-hidden="true">
          {t("common.appName")}
        </div>
        <ul className={styles.navList}>
          {NAV.map((item) => (
            <li key={item.to}>
              <NavLink to={item.to} className={styles.navLink ?? ""}>
                <Icon name={item.icon} />
                {item.label()}
              </NavLink>
            </li>
          ))}
        </ul>
      </nav>
      <div className={styles.column}>
        <header className={styles.topbar} data-nav-group="">
          <ServerSwitcher servers={servers.data ?? []} active={server} />
          <div className={styles.spacer} />
          <DownloadIndicator />
          <AccountMenu server={server} />
        </header>
        <UpdateBannerSlot />
        {offline.has(server.id) ? (
          <div className={styles.banner} role="status">
            <span className={styles.bannerIcon}>
              <Icon name="offline" />
            </span>
            <div>
              <strong>{t("shell.offlineTitle")}</strong>
              <p>{t("shell.offlineText", { server: server.name })}</p>
            </div>
          </div>
        ) : null}
        <main id="main-content" className={styles.content} tabIndex={-1} data-nav-group="">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
