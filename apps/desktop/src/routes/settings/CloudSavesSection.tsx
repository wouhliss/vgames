// Cloud saves (06-cloud-saves §3): which installed games keep saves in the cloud, their sync state, the
// conflict that needs a decision, and each game's history (server snapshots and local backups) with Restore.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useInstalls } from "../../app/queries";
import { Button } from "../../components/Button";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { Dialog } from "../../components/Dialog";
import { Badge, EmptyState, ErrorState, LoadingState } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { useToast } from "../../components/Toast";
import { formatBytes, formatDateTime, t } from "../../i18n";
import {
  commands,
  type InstalledPackage,
  type SaveBackup,
  type SaveRestoreError,
  type SaveSnapshot,
  type SaveSource,
} from "../../ipc";
import { commandError, queryKeys, unwrap } from "../../ipc/query";
import { useSaveConflicts } from "../saves/SaveSyncHost";
import savesStyles from "../saves/Saves.module.css";
import styles from "./Settings.module.css";
import { Section } from "./SettingsPage";

export function restoreErrorText(error: SaveRestoreError, title: string): string {
  switch (error.kind) {
    case "not_found":
      return t("saves.settings.restoreErrors.notFound");
    case "running":
      return t("saves.settings.restoreErrors.running", { title });
    case "offline":
      return t("saves.settings.restoreErrors.offline");
    case "io":
      return t("saves.settings.restoreErrors.io", { detail: error.detail });
  }
}

const STATE_TONE = {
  synced: "success",
  syncing: "info",
  pending: "warning",
  conflict: "danger",
} as const;

export function CloudSavesSection() {
  const installs = useInstalls();
  const [openHistory, setOpenHistory] = useState<InstalledPackage | null>(null);
  const { openConflict } = useSaveConflicts();

  let body: React.ReactNode;
  if (installs.data) {
    const games = installs.data.filter((i) => i.cloud_saves !== "unsupported");
    body =
      games.length === 0 ? (
        <EmptyState title={t("saves.settings.empty")} description={t("saves.settings.emptyHint")} />
      ) : (
        <ul className={styles.rows} data-nav-group="">
          {games.map((pkg) => {
            const state = pkg.cloud_saves === "unsupported" ? "synced" : pkg.cloud_saves;
            return (
              <li key={pkg.package.package_id} className={styles.row}>
                <div className={styles.rowMain}>
                  <span className={styles.rowTitle}>{pkg.title}</span>
                  <span>
                    <Badge tone={STATE_TONE[state]}>{t(`saves.settings.state.${state}`)}</Badge>
                  </span>
                </div>
                <div className={styles.cardHeader}>
                  {state === "conflict" ? (
                    <Button
                      variant="primary"
                      aria-label={t("saves.settings.resolveConflictFor", { title: pkg.title })}
                      onClick={() => openConflict(`conflict-${pkg.package.package_id}`)}
                    >
                      {t("saves.settings.resolveConflict")}
                    </Button>
                  ) : null}
                  <Button
                    aria-label={t("saves.settings.historyFor", { title: pkg.title })}
                    onClick={() => setOpenHistory(pkg)}
                  >
                    {t("saves.settings.history")}
                  </Button>
                </div>
              </li>
            );
          })}
        </ul>
      );
  } else if (installs.isError) {
    body = <ErrorState title={t("error.generic")} onRetry={() => void installs.refetch()} />;
  } else body = <LoadingState />;

  return (
    <Section
      id="cloud-saves"
      title={t("settings.section.cloudSaves")}
      text={t("saves.settings.text")}
    >
      {body}
      {openHistory ? (
        <HistoryDialog pkg={openHistory} onClose={() => setOpenHistory(null)} />
      ) : null}
    </Section>
  );
}

function HistoryDialog({ pkg, onClose }: { pkg: InstalledPackage; onClose: () => void }) {
  const client = useQueryClient();
  const { toast } = useToast();
  const query = useQuery({
    queryKey: queryKeys.saveHistory(pkg.package.package_id),
    queryFn: async () => unwrap(await commands.savesHistory(pkg.package)),
  });
  const [pending, setPending] = useState<{
    source: SaveSource;
    when: string;
    files: number;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);

  const restore = async () => {
    if (!pending) return;
    const result = await commands.savesRestore(pkg.package, pending.source);
    setPending(null);
    if (result.status === "error") {
      setError(restoreErrorText(result.error, pkg.title));
      return;
    }
    setError(null);
    void client.invalidateQueries({ queryKey: queryKeys.saveHistories });
    void client.invalidateQueries({ queryKey: queryKeys.installs });
    toast({ tone: "success", title: t("saves.settings.restored", { title: pkg.title }) });
  };

  const history = query.data;
  // The server's list is empty while it can't be reached; backups are local and still shown.
  const offline = commandError<{ kind: string }>(query.error)?.kind === "offline";

  let body: React.ReactNode;
  if (history) {
    body = (
      <>
        {error ? (
          <Notice tone="danger" role="alert">
            {error}
          </Notice>
        ) : null}
        <h3 className={savesStyles.subhead}>{t("saves.settings.snapshots")}</h3>
        {history.snapshots.length === 0 ? (
          <p>{offline ? t("saves.settings.offlineSnapshots") : t("saves.settings.noSnapshots")}</p>
        ) : (
          <ul className={styles.rows} data-nav-group="">
            {history.snapshots.map((s) => (
              <SnapshotRow
                key={s.snapshot_id}
                snapshot={s}
                onRestore={() =>
                  setPending({
                    source: { kind: "snapshot", snapshot_id: s.snapshot_id },
                    when: formatDateTime(s.created_at),
                    files: s.file_count,
                  })
                }
              />
            ))}
          </ul>
        )}
        <h3 className={savesStyles.subhead}>{t("saves.settings.backups")}</h3>
        {history.backups.length === 0 ? (
          <p>{t("saves.settings.noBackups")}</p>
        ) : (
          <ul className={styles.rows} data-nav-group="">
            {history.backups.map((b) => (
              <BackupRow
                key={b.backup_id}
                backup={b}
                onRestore={() =>
                  setPending({
                    source: { kind: "backup", backup_id: b.backup_id },
                    when: formatDateTime(b.created_at),
                    files: b.file_count,
                  })
                }
              />
            ))}
          </ul>
        )}
      </>
    );
  } else if (query.isError) {
    body = (
      <ErrorState title={t("saves.settings.loadFailed")} onRetry={() => void query.refetch()} />
    );
  } else body = <LoadingState />;

  return (
    <>
      <Dialog
        open={!pending}
        size="lg"
        title={t("saves.settings.historyFor", { title: pkg.title })}
        onClose={onClose}
        footer={<Button onClick={onClose}>{t("common.close")}</Button>}
      >
        {body}
      </Dialog>
      <ConfirmDialog
        open={pending !== null}
        title={t("saves.settings.restoreTitle")}
        description={
          pending
            ? t("saves.settings.restoreText", {
                when: pending.when,
                files: t("saves.conflict.files", { count: pending.files }),
              })
            : ""
        }
        confirmLabel={t("saves.settings.restoreConfirm")}
        onConfirm={() => void restore()}
        onCancel={() => setPending(null)}
      />
    </>
  );
}

function SnapshotRow({ snapshot, onRestore }: { snapshot: SaveSnapshot; onRestore: () => void }) {
  const when = formatDateTime(snapshot.created_at);
  return (
    <li className={styles.row}>
      <div className={styles.rowMain}>
        <span className={styles.rowTitle}>{when}</span>
        <span>
          {t("saves.settings.savedBy", { device: snapshot.device_name })} ·{" "}
          {t("saves.conflict.files", { count: snapshot.file_count })} ·{" "}
          {formatBytes(snapshot.size_bytes)}
        </span>
      </div>
      {snapshot.current ? (
        <Badge tone="accent">{t("saves.settings.current")}</Badge>
      ) : (
        <Button aria-label={t("saves.settings.restoreLabel", { when })} onClick={onRestore}>
          {t("saves.settings.restore")}
        </Button>
      )}
    </li>
  );
}

function BackupRow({ backup, onRestore }: { backup: SaveBackup; onRestore: () => void }) {
  const when = formatDateTime(backup.created_at);
  return (
    <li className={styles.row}>
      <div className={styles.rowMain}>
        <span className={styles.rowTitle}>{when}</span>
        <span>
          {t(`saves.settings.reason.${backup.reason}`)} ·{" "}
          {t("saves.conflict.files", { count: backup.file_count })} ·{" "}
          {formatBytes(backup.size_bytes)}
        </span>
      </div>
      <Button aria-label={t("saves.settings.restoreLabel", { when })} onClick={onRestore}>
        {t("saves.settings.restore")}
      </Button>
    </li>
  );
}
