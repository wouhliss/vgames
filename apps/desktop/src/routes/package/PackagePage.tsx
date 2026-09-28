// Package details: hero, what to do next (Install, or Play/Update when installed), facts about the
// current release, the description (safe Markdown), screenshots with a viewer, and compatibility.
import { useQuery } from "@tanstack/react-query";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { useCollections, useInstalls, useLibraries } from "../../app/queries";
import { Button } from "../../components/Button";
import { Badge, EmptyState, ErrorState, LoadingState } from "../../components/Feedback";
import { SafeMarkdown } from "../../components/SafeMarkdown";
import { Tooltip } from "../../components/Tooltip";
import { formatBytes, formatDate, t } from "../../i18n";
import { type CatalogError, commands, type InstalledPackage, type PackageDetails } from "../../ipc";
import { CommandError, queryKeys } from "../../ipc/query";
import { InviteDialog } from "../friends/InviteDialog";
import { LibraryActionsProvider } from "../library/actions";
import { initials, placeholderHue } from "../library/model";
import { MoreMenu, PackageBadges, PrimaryButton } from "../library/Tile";
import { CompatPanel } from "./CompatPanel";
import { InstallDialog } from "./InstallDialog";
import { Lightbox } from "./Lightbox";
import { blockerText, isHardBlocker } from "./messages";
import styles from "./Package.module.css";

export function PackagePage() {
  const { packageId = "" } = useParams();
  const navigate = useNavigate();
  const details = useQuery({
    queryKey: queryKeys.details(packageId),
    queryFn: async () => {
      const result = await commands.packageDetails(packageId);
      if (result.status === "error") throw new CommandError(result.error);
      return result.data;
    },
  });
  const installs = useInstalls();
  const installed = installs.data?.find((i) => i.package.package_id === packageId) ?? null;
  const error =
    details.error instanceof CommandError ? (details.error.error as CatalogError) : null;
  const removed = error?.kind === "not_found";
  const titleRef = useRef<HTMLHeadingElement>(null);

  // Found out that the package is gone while the player was on the page: the button they used
  // disappeared, so move focus to the page title instead of leaving it nowhere.
  useEffect(() => {
    if (removed && (document.activeElement === null || document.activeElement === document.body))
      titleRef.current?.focus();
  }, [removed]);

  const pkg = details.data && !removed ? details.data : null;
  let body: ReactNode;
  if (removed) {
    body = (
      <EmptyState
        icon="info"
        title={t("package.removedTitle")}
        description={t("package.removedText")}
        action={
          <Button variant="primary" icon="browse" onClick={() => navigate("/browse")}>
            {t("package.toBrowse")}
          </Button>
        }
      />
    );
  } else if (pkg) {
    body = <Details pkg={pkg} />;
  } else if (details.isError) {
    body = <ErrorState title={t("package.loadError")} onRetry={() => void details.refetch()} />;
  } else {
    body = <LoadingState />;
  }

  // One h1 through loading, content and removal, so focus on it survives the switch.
  return (
    <div className={styles.page}>
      <div>
        <Button variant="ghost" icon="arrowLeft" onClick={() => navigate(-1)}>
          {t("package.back")}
        </Button>
      </div>
      {pkg ? <Hero pkg={pkg} /> : null}
      <div className={pkg ? styles.headline : undefined}>
        <h1
          ref={titleRef}
          tabIndex={-1}
          data-page-title=""
          className={pkg ? undefined : "visually-hidden"}
        >
          {details.data?.title ?? t("package.pageTitle")}
        </h1>
        {pkg ? <Headline pkg={pkg} installed={installed} /> : null}
      </div>
      {body}
    </div>
  );
}

function Hero({ pkg }: { pkg: PackageDetails }) {
  const hue = placeholderHue(pkg.package_id);
  const [broken, setBroken] = useState(false);
  return pkg.hero_url && !broken ? (
    <img className={styles.hero} src={pkg.hero_url} alt="" onError={() => setBroken(true)} />
  ) : (
    <div
      className={`${styles.hero} ${styles.heroPlaceholder}`}
      style={{
        background: `linear-gradient(120deg, hsl(${hue} 45% 30%), hsl(${(hue + 60) % 360} 50% 12%))`,
      }}
      aria-hidden="true"
    >
      {initials(pkg.title)}
    </div>
  );
}

/** Summary, badges and what to do next: Install, or Play and the library actions when installed. */
function Headline({ pkg, installed }: { pkg: PackageDetails; installed: InstalledPackage | null }) {
  const [installing, setInstalling] = useState(false);
  const [inviting, setInviting] = useState(false);
  const blockers =
    pkg.compat.kind === "compat" || pkg.compat.kind === "rosetta" ? pkg.compat.blockers : [];
  const hardBlocker = blockers.find(isHardBlocker) ?? null;
  return (
    <>
      {pkg.summary ? <p className={styles.summary}>{pkg.summary}</p> : null}
      <div className={styles.badgeRow}>
        {pkg.release && pkg.release.via !== "native" ? (
          <Badge tone="accent">{t(`availability.${pkg.release.via}`)}</Badge>
        ) : null}
        {pkg.compat.kind === "unavailable" ? (
          <Badge tone="warning">{t("availability.unavailable")}</Badge>
        ) : null}
        {installed ? <InstalledBadges pkg={installed} /> : null}
      </div>
      <div className={styles.actions}>
        {installed ? (
          <InstalledActions pkg={installed} />
        ) : pkg.compat.kind === "unavailable" || !pkg.release ? (
          <Button variant="primary" size="lg" icon="download" aria-disabled>
            {t("package.notAvailable")}
          </Button>
        ) : hardBlocker ? (
          <Tooltip content={blockerText(hardBlocker)} describe>
            <Button
              variant="primary"
              size="lg"
              icon="download"
              aria-disabled
              aria-label={t("package.installTitle", { title: pkg.title })}
            >
              {t("package.install")}
            </Button>
          </Tooltip>
        ) : (
          <Button
            variant="primary"
            size="lg"
            icon="download"
            aria-label={t("package.installTitle", { title: pkg.title })}
            onClick={() => setInstalling(true)}
          >
            {t("package.install")}
          </Button>
        )}
        {pkg.release ? (
          <Button icon="friends" onClick={() => setInviting(true)}>
            {t("friends.inviteToPlay")}
          </Button>
        ) : null}
        {installed ? (
          <span className={styles.muted}>
            {t("package.installedIn", { version: installed.version_label })}
          </span>
        ) : null}
      </div>
      {pkg.compat.kind === "unavailable" ? (
        <p className={styles.muted}>{t("package.notAvailableText")}</p>
      ) : hardBlocker && !installed ? (
        <p className={styles.muted}>{blockerText(hardBlocker)}</p>
      ) : null}
      {installing ? (
        <InstallDialog
          packageId={pkg.package_id}
          title={pkg.title}
          onClose={() => setInstalling(false)}
        />
      ) : null}
      {inviting ? (
        <InviteDialog
          pkg={{ id: pkg.package_id, title: pkg.title }}
          onClose={() => setInviting(false)}
        />
      ) : null}
    </>
  );
}

/** Description, screenshots, compatibility and the facts about the current release. */
function Details({ pkg }: { pkg: PackageDetails }) {
  const [shot, setShot] = useState<number | null>(null);
  const facts: [string, ReactNode][] = [
    [t("package.version"), pkg.release ? pkg.release.version_label : t("package.unknown")],
    [t("package.size"), pkg.release ? formatBytes(pkg.release.total_size) : t("package.unknown")],
    [t("package.platforms"), pkg.platforms.map((p) => t(`platform.${p}`)).join(", ")],
    [t("package.developer"), pkg.developer ?? t("package.unknown")],
    [t("package.publisher"), pkg.publisher ?? t("package.unknown")],
    [t("package.released"), pkg.release_date ? formatDate(pkg.release_date) : t("package.unknown")],
    [t("package.genres"), pkg.genres.length > 0 ? pkg.genres.join(", ") : t("package.unknown")],
  ];

  return (
    <>
      <div className={styles.columns}>
        <div className={styles.main}>
          <section className={styles.section} aria-labelledby="about-title">
            <h2 id="about-title">{t("package.about")}</h2>
            {pkg.description ? (
              <SafeMarkdown source={pkg.description} headingOffset={2} />
            ) : (
              <p className={styles.muted}>{t("package.noDescription")}</p>
            )}
          </section>
          {pkg.screenshots.length > 0 ? (
            <section className={styles.section} aria-labelledby="shots-title">
              <h2 id="shots-title">{t("package.screenshots")}</h2>
              <ul className={styles.shots} data-nav-group="">
                {pkg.screenshots.map((s, i) => (
                  <li key={s.url}>
                    <button
                      type="button"
                      className={styles.thumb}
                      onClick={() => setShot(i)}
                      aria-label={t("package.screenshotLabel", {
                        n: i + 1,
                        total: pkg.screenshots.length,
                      })}
                    >
                      <img src={s.url} alt="" loading="lazy" width={s.width} height={s.height} />
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          ) : null}
          <CompatPanel compat={pkg.compat} packageId={pkg.package_id} />
        </div>
        <aside className={styles.aside} aria-labelledby="facts-title">
          <h2 id="facts-title">{t("package.details")}</h2>
          <dl className={styles.facts}>
            {facts.map(([label, value]) => (
              <div key={label}>
                <dt>{label}</dt>
                <dd>{value}</dd>
              </div>
            ))}
          </dl>
        </aside>
      </div>
      {shot !== null ? (
        <Lightbox
          title={pkg.title}
          screenshots={pkg.screenshots}
          index={shot}
          onIndex={setShot}
          onClose={() => setShot(null)}
        />
      ) : null}
    </>
  );
}

function InstalledBadges({ pkg }: { pkg: InstalledPackage }) {
  const libraries = useLibraries();
  const offline = libraries.data?.find((l) => l.id === pkg.library_id)?.online === false;
  return <PackageBadges pkg={pkg} offline={offline} />;
}

const HIDDEN_HERE = ["details"];

/** Play / Stop / Resume and the same actions menu as in the library (minus "Details"). */
function InstalledActions({ pkg }: { pkg: InstalledPackage }) {
  const libraries = useLibraries();
  const collections = useCollections();
  const installs = useInstalls();
  return (
    <LibraryActionsProvider
      libraries={libraries.data ?? []}
      collections={collections.data ?? []}
      installs={installs.data ?? []}
    >
      <span className={styles.installedActions}>
        <PrimaryButton pkg={pkg} size="lg" />
        <MoreMenu pkg={pkg} omit={HIDDEN_HERE} size="md" />
      </span>
    </LibraryActionsProvider>
  );
}
