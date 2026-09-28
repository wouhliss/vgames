// Settings: a list of sections on the left, the chosen section on the right. Each section is its own
// URL (/settings/<section>), so links (e.g. "Open Storage settings") can point straight at one.
import type { ReactNode } from "react";
import { Navigate, NavLink, useParams } from "react-router";
import { t } from "../../i18n";
import { Page } from "../Page";
import { AboutSection } from "./AboutSection";
import { AccountSection } from "./AccountSection";
import { DownloadsSection } from "./DownloadsSection";
import { GeneralSection } from "./GeneralSection";
import { OverlaySection } from "./OverlaySection";
import { PrivacySection } from "./PrivacySection";
import { ServersSection } from "./ServersSection";
import styles from "./Settings.module.css";
import { StorageSection } from "./StorageSection";
import { UpdatesSection } from "./UpdatesSection";

const SECTIONS = {
  general: GeneralSection,
  servers: ServersSection,
  account: AccountSection,
  storage: StorageSection,
  downloads: DownloadsSection,
  privacy: PrivacySection,
  overlay: OverlaySection,
  updates: UpdatesSection,
  about: AboutSection,
} as const;

type SectionId = keyof typeof SECTIONS;

const isSection = (id: string): id is SectionId => id in SECTIONS;

export function SettingsPage() {
  const section = useParams()["*"] ?? "";
  if (!isSection(section)) return <Navigate to="/settings/general" replace />;
  const Section = SECTIONS[section];
  return (
    <Page title={t("settings.title")}>
      <div className={styles.layout}>
        <nav aria-label={t("settings.sections")} className={styles.nav} data-nav-group="">
          <ul>
            {(Object.keys(SECTIONS) as SectionId[]).map((id) => (
              <li key={id}>
                <NavLink to={`/settings/${id}`} className={styles.navLink ?? ""}>
                  {t(`settings.section.${id}`)}
                </NavLink>
              </li>
            ))}
          </ul>
        </nav>
        <div className={styles.content}>
          <Section />
        </div>
      </div>
    </Page>
  );
}

/** A section's frame: its heading (h2) and an optional line of explanation. */
export function Section({
  id,
  title,
  text,
  children,
}: {
  id: string;
  title: string;
  text?: string;
  children: ReactNode;
}) {
  return (
    <section aria-labelledby={`settings-${id}`} className={styles.section}>
      <h2 id={`settings-${id}`}>{title}</h2>
      {text ? <p className={styles.muted}>{text}</p> : null}
      {children}
    </section>
  );
}
