// What's new (08-release §2–3): every release between the installed version and the update, grouped
// by type, as plain text. "Install and restart" says why when it can't be used.
import { useQuery } from "@tanstack/react-query";
import { useId, useState } from "react";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { ErrorState, LoadingState } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { formatDate, t } from "../../i18n";
import { commands, type ReleaseNotes, type UpdaterStatus } from "../../ipc";
import { queryKeys, unwrap } from "../../ipc/query";
import { groupEntries, installBlock } from "./model";
import styles from "./Update.module.css";

function Release({ release, grouped }: { release: ReleaseNotes; grouped: boolean }) {
  const headingId = useId();
  const groups = grouped ? groupEntries(release.entries) : [];
  return (
    <section aria-labelledby={headingId} className={styles.release}>
      <h3 id={headingId}>{t("update.releaseHeading", { version: release.version })}</h3>
      {release.date ? (
        <p className={styles.muted}>
          {t("update.releaseDate", { date: formatDate(release.date) })}
        </p>
      ) : null}
      {release.entries.length === 0 ? (
        <p>{t("update.generic")}</p>
      ) : grouped ? (
        groups.map((group) => (
          <div key={group.type} className={styles.group}>
            <h4>{t(`update.group.${group.type}`)}</h4>
            <ul>
              {group.texts.map((text, i) => (
                // biome-ignore lint/suspicious/noArrayIndexKey: entries are positional and may repeat.
                <li key={i}>{text}</li>
              ))}
            </ul>
          </div>
        ))
      ) : (
        <ul>
          {release.entries.map((entry, i) => (
            // biome-ignore lint/suspicious/noArrayIndexKey: entries are positional and may repeat.
            <li key={i}>{entry.text}</li>
          ))}
        </ul>
      )}
    </section>
  );
}

export function WhatsNewDialog({
  open,
  onClose,
  version,
  status,
}: {
  open: boolean;
  onClose: () => void;
  version: string;
  status: UpdaterStatus | undefined;
}) {
  const notes = useQuery({
    queryKey: queryKeys.whatsNew(version),
    // Loaded as soon as an update is known (the core caches it), so the dialog opens on the notes.
    queryFn: async () => unwrap(await commands.updaterWhatsNew()),
  });
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const reasonId = useId();
  const notesLabel = t("update.whatsNew");
  const block = installBlock(status, pending);

  const install = async () => {
    if (block) return;
    setPending(true);
    setError(null);
    try {
      const result = await commands.updaterInstall();
      if (result.status === "error")
        setError(t("update.installFailed", { detail: result.error.message }));
    } catch {
      setError(t("update.installFailed", { detail: t("error.generic") }));
    } finally {
      setPending(false);
    }
  };

  const reason =
    block === "game_running"
      ? t("update.blockedGame")
      : block === "installing"
        ? t("update.busy")
        : status?.blocked === "downloads_active"
          ? t("update.blockedDownloads")
          : null;

  return (
    <Dialog
      open={open}
      onClose={onClose}
      size="lg"
      title={t("update.dialogTitle", { version })}
      footer={
        <>
          {reason ? (
            <p id={reasonId} className={styles.reason}>
              {reason}
            </p>
          ) : null}
          <Button variant="ghost" onClick={onClose}>
            {t("update.later")}
          </Button>
          <Button
            variant="primary"
            icon="refresh"
            loading={block === "installing"}
            aria-disabled={block ? true : undefined}
            aria-describedby={reason ? reasonId : undefined}
            onClick={() => void install()}
          >
            {block === "installing" ? t("update.installing") : t("update.install")}
          </Button>
        </>
      }
    >
      {error ? (
        <Notice tone="danger" role="alert">
          {error}
        </Notice>
      ) : null}
      {notes.isPending ? (
        <LoadingState />
      ) : notes.isError ? (
        <ErrorState title={t("update.loadFailed")} onRetry={() => void notes.refetch()} />
      ) : (
        // biome-ignore lint/a11y/noNoninteractiveTabindex: the scrollable notes must be reachable with the keyboard and the D-pad.
        <section className={styles.notes} aria-label={notesLabel} tabIndex={0} data-autofocus="">
          {notes.data.releases.map((release) => (
            <Release
              key={release.version}
              release={release}
              grouped={!notes.data.from_latest_notes}
            />
          ))}
        </section>
      )}
    </Dialog>
  );
}
