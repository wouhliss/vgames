// A library entry as a grid tile or a list row. The cover/title button opens the details page and
// can be dragged onto a collection; Play (or the current state's action) and the menu sit below.
// Right-click, Shift+F10 or the controller's menu button open the same actions as the "…" button.
import { type DragEvent, memo, useId, useState } from "react";
import { Button, IconButton } from "../../components/Button";
import { Badge } from "../../components/Feedback";
import { Icon } from "../../components/Icon";
import { ContextMenu, Menu } from "../../components/Menu";
import { Tooltip } from "../../components/Tooltip";
import { formatBytes, formatRelativeTime, t } from "../../i18n";
import type { InstalledPackage } from "../../ipc";
import { useLibraryActionsContext } from "./actions";
import styles from "./Library.module.css";
import { initials, placeholderHue } from "./model";

export const PACKAGE_DRAG_TYPE = "application/x-vgames-package";

function onDragStart(e: DragEvent<HTMLElement>, pkg: InstalledPackage) {
  e.dataTransfer.setData(PACKAGE_DRAG_TYPE, JSON.stringify(pkg.package));
  e.dataTransfer.setData("text/plain", pkg.title);
  e.dataTransfer.effectAllowed = "copy";
}

export function Cover({ pkg, size = "tile" }: { pkg: InstalledPackage; size?: "tile" | "thumb" }) {
  const [broken, setBroken] = useState(false);
  const className = size === "tile" ? styles.cover : styles.thumb;
  if (pkg.cover_url && !broken) {
    return (
      <img
        className={className}
        src={pkg.cover_url}
        alt=""
        loading="lazy"
        decoding="async"
        draggable={false}
        onError={() => setBroken(true)}
      />
    );
  }
  const hue = placeholderHue(pkg.package.package_id);
  return (
    <span
      className={`${className} ${styles.placeholder}`}
      style={{
        background: `linear-gradient(160deg, hsl(${hue} 45% 32%), hsl(${(hue + 40) % 360} 55% 16%))`,
      }}
      aria-hidden="true"
    >
      {initials(pkg.title)}
    </span>
  );
}

export function PackageBadges({ pkg, offline }: { pkg: InstalledPackage; offline: boolean }) {
  return (
    <>
      {pkg.running ? (
        <Badge tone="success" icon="play">
          {t("library.badges.running")}
        </Badge>
      ) : null}
      {offline ? (
        <Badge tone="warning" icon="offline">
          {t("library.badges.offline")}
        </Badge>
      ) : null}
      {pkg.state === "incomplete" ? (
        <Badge tone="warning" icon="warning">
          {t("library.badges.incomplete")}
        </Badge>
      ) : null}
      {pkg.state !== "installed" && pkg.state !== "incomplete" ? (
        <Badge tone="info">{t(`library.badges.${pkg.state}`)}</Badge>
      ) : null}
      {pkg.update && pkg.state === "installed" ? (
        <Badge tone="accent" icon="download">
          {pkg.update.installed_yanked
            ? t("library.badges.updateRequired")
            : t("library.badges.update")}
        </Badge>
      ) : null}
      {pkg.cloud_saves === "conflict" ? (
        <Badge tone="danger" icon="cloudOff">
          {t("library.badges.saveConflict")}
        </Badge>
      ) : pkg.cloud_saves === "pending" ? (
        <Badge tone="warning" icon="cloudOff">
          {t("library.badges.syncPending")}
        </Badge>
      ) : pkg.cloud_saves === "syncing" ? (
        <Badge tone="info" icon="cloud">
          {t("library.badges.syncing")}
        </Badge>
      ) : null}
      {pkg.compat !== "native" ? (
        <Badge tone="neutral">
          {pkg.compat === "proton" ? t("library.badges.proton") : t("library.badges.wine")}
        </Badge>
      ) : null}
    </>
  );
}

function PrimaryButton({ pkg }: { pkg: InstalledPackage }) {
  const actions = useLibraryActionsContext();
  const action = actions.primary(pkg);
  const run = () => actions.runPrimary(pkg);
  switch (action.kind) {
    case "play":
      return (
        <Button
          variant="primary"
          size="sm"
          icon="play"
          loading={actions.launching.has(pkg.package.package_id)}
          aria-label={t("library.actions.playTitle", { title: pkg.title })}
          onClick={run}
        >
          {t("library.actions.play")}
        </Button>
      );
    case "offline":
      return (
        <Tooltip content={action.reason} describe>
          <Button
            variant="primary"
            size="sm"
            icon="play"
            aria-disabled
            aria-label={t("library.actions.playTitle", { title: pkg.title })}
          >
            {t("library.actions.play")}
          </Button>
        </Tooltip>
      );
    case "stop":
      return (
        <Button
          variant="danger"
          size="sm"
          icon="stop"
          aria-label={t("library.actions.stopTitle", { title: pkg.title })}
          onClick={run}
        >
          {t("library.actions.stop")}
        </Button>
      );
    case "resume":
      return (
        <Button
          size="sm"
          icon="download"
          aria-label={t("library.actions.resumeTitle", { title: pkg.title })}
          onClick={run}
        >
          {t("library.actions.resume")}
        </Button>
      );
    case "download":
      return (
        <Button size="sm" icon="download" onClick={run}>
          {t("library.actions.viewDownload")}
        </Button>
      );
    case "busy":
      return (
        <Button size="sm" aria-disabled>
          {action.label}
        </Button>
      );
  }
}

function MoreMenu({ pkg }: { pkg: InstalledPackage }) {
  const actions = useLibraryActionsContext();
  return (
    <Menu
      label={t("library.actions.menu", { title: pkg.title })}
      entries={actions.entries(pkg)}
      trigger={
        <IconButton icon="more" size="sm" label={t("library.actions.more", { title: pkg.title })} />
      }
    />
  );
}

function playedText(pkg: InstalledPackage): string {
  if (pkg.state === "incomplete") return t("library.meta.notInstalled");
  return pkg.last_played_at
    ? t("library.meta.played", { when: formatRelativeTime(pkg.last_played_at) })
    : t("library.meta.neverPlayed");
}

export const GridTile = memo(function GridTile({
  pkg,
  offline,
}: {
  pkg: InstalledPackage;
  offline: boolean;
}) {
  const actions = useLibraryActionsContext();
  const id = useId();
  return (
    <ContextMenu
      label={t("library.actions.menu", { title: pkg.title })}
      entries={actions.entries(pkg)}
    >
      <article className={styles.tile} aria-labelledby={`${id}-title`} data-package={pkg.slug}>
        <button
          type="button"
          className={styles.coverButton}
          aria-describedby={`${id}-badges`}
          draggable
          onDragStart={(e) => onDragStart(e, pkg)}
          onClick={() => actions.openDetails(pkg)}
        >
          <Cover pkg={pkg} />
          <span className={styles.tileTitle}>
            {pkg.favorite ? (
              <span className={styles.favorite}>
                <Icon name="star" size={14} filled label={t("library.badges.favorite")} />
              </span>
            ) : null}{" "}
            <span id={`${id}-title`} className={styles.ellipsis}>
              {pkg.title}
            </span>
          </span>
        </button>
        <div id={`${id}-badges`} className={styles.badgeOverlay}>
          <PackageBadges pkg={pkg} offline={offline} />
        </div>
        <div className={styles.tileMeta}>{playedText(pkg)}</div>
        <div className={styles.tileActions}>
          <PrimaryButton pkg={pkg} />
          <MoreMenu pkg={pkg} />
        </div>
      </article>
    </ContextMenu>
  );
});

export const ListRow = memo(function ListRow({
  pkg,
  offline,
}: {
  pkg: InstalledPackage;
  offline: boolean;
}) {
  const actions = useLibraryActionsContext();
  const id = useId();
  return (
    <ContextMenu
      label={t("library.actions.menu", { title: pkg.title })}
      entries={actions.entries(pkg)}
    >
      <article className={styles.row} aria-labelledby={`${id}-title`} data-package={pkg.slug}>
        <button
          type="button"
          className={styles.rowMain}
          aria-describedby={`${id}-badges`}
          draggable
          onDragStart={(e) => onDragStart(e, pkg)}
          onClick={() => actions.openDetails(pkg)}
        >
          <Cover pkg={pkg} size="thumb" />
          <span className={styles.rowText}>
            <span className={styles.rowTitle}>
              {pkg.favorite ? (
                <span className={styles.favorite}>
                  <Icon name="star" size={14} filled label={t("library.badges.favorite")} />
                </span>
              ) : null}{" "}
              <span id={`${id}-title`} className={styles.ellipsis}>
                {pkg.title}
              </span>
            </span>
            <span className={styles.rowMeta}>{playedText(pkg)}</span>
          </span>
        </button>
        <div id={`${id}-badges`} className={styles.rowBadges}>
          <PackageBadges pkg={pkg} offline={offline} />
        </div>
        <span className={styles.rowSize}>{formatBytes(pkg.size_bytes)}</span>
        <div className={styles.rowActions}>
          <PrimaryButton pkg={pkg} />
          <MoreMenu pkg={pkg} />
        </div>
      </article>
    </ContextMenu>
  );
});
