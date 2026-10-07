// Every version of the chosen package, newest first, with release (ready) and withdraw (released).
import { Button } from "../../components/Button";
import { Badge, type BadgeTone } from "../../components/Feedback";
import { formatBytes, formatDateTime, t } from "../../i18n";
import type { PublishVersion, VersionState } from "../../ipc";
import { platformLabel } from "./model";
import styles from "./Publish.module.css";

const TONE: Record<VersionState, BadgeTone> = {
  uploading: "info",
  verifying: "info",
  ready: "accent",
  published: "success",
  failed: "danger",
  yanked: "warning",
  aborted: "neutral",
};

export function Versions({
  versions,
  onRelease,
  onYank,
}: {
  versions: readonly PublishVersion[];
  onRelease: (version: PublishVersion) => void;
  onYank: (version: PublishVersion) => void;
}) {
  if (versions.length === 0) return <p className={styles.muted}>{t("publish.noVersions")}</p>;
  return (
    <ul className={styles.versions}>
      {versions.map((v) => (
        <li key={v.id} className={styles.version}>
          <div className={styles.versionHead}>
            <h3 className={styles.jobTitle}>{v.version_label}</h3>
            <Badge tone="neutral">{platformLabel(v.platform)}</Badge>
            <Badge tone={TONE[v.state]}>{t(`publish.state.${v.state}`)}</Badge>
            {v.is_current_release ? <Badge tone="success">{t("publish.current")}</Badge> : null}
          </div>
          <p className={styles.muted}>
            {[
              v.total_size !== null ? formatBytes(v.total_size) : null,
              formatDateTime(v.published_at ?? v.created_at),
            ]
              .filter(Boolean)
              .join(" · ")}
          </p>
          {v.failure_reason ? <p className={styles.error}>{v.failure_reason}</p> : null}
          {v.state === "ready" || v.state === "published" ? (
            <div className={styles.actions}>
              {v.state === "ready" ? (
                <Button variant="primary" icon="cloud" onClick={() => onRelease(v)}>
                  {t("publish.releaseConfirm")} {v.version_label}
                </Button>
              ) : (
                <Button variant="danger" icon="block" onClick={() => onYank(v)}>
                  {t("publish.yank", { version: v.version_label })}
                </Button>
              )}
            </div>
          ) : null}
        </li>
      ))}
    </ul>
  );
}
