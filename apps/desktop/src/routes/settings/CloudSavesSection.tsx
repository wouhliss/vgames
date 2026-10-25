// Cloud saves (06 §3, 06-cloud-saves-notes): each game with cloud saves, its sync state, and its
// history (cloud snapshots and this device's backups) with a confirmed Restore. Restoring backs the
// current files up first and never starts the game.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useState } from "react";
import { useInstalls } from "../../app/queries";
import { Button } from "../../components/Button";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { Dialog } from "../../components/Dialog";
import { Badge, type BadgeTone, ErrorState, LoadingState } from "../../components/Feedback";
import { useToast } from "../../components/Toast";
import { formatBytes, formatDateTime, t } from "../../i18n";
import {
  type AppError,
  type CloudSaveState,
  commands,
  type InstalledPackage,
  type SaveSummary,
} from "../../ipc";
import { queryKeys, unwrap } from "../../ipc/query";
import styles from "./Settings.module.css";
import { Section } from "./SettingsPage";

const STATE: Record<
  Exclude<CloudSaveState, "unsupported">,
  { tone: BadgeTone; label: () => string }
> = {
  synced: { tone: "success", label: () => t("settings.cloudSaves.state.synced") },
  syncing: { tone: "info", label: () => t("settings.cloudSaves.state.syncing") },
  pending: { tone: "warning", label: () => t("settings.cloudSaves.state.pending") },
  conflict: { tone: "danger", label: () => t("settings.cloudSaves.state.conflict") },
};

export function CloudSavesSection() {
  const installs = useInstalls();
  const [shown, setShown] = useState<InstalledPackage | null>(null);

  let body: ReactNode;
  if (installs.data) {
    const games = installs.data.filter((pkg) => pkg.cloud_saves !== "unsupported");
    body =
      games.length === 0 ? (
        <p className={styles.muted}>{t("settings.cloudSaves.empty")}</p>
      ) : (
        <ul className={styles.rows} data-nav-group="">
          {games.map((pkg) => {
            const state = STATE[pkg.cloud_saves as keyof typeof STATE];
            return (
              <li key={`${pkg.package.server_id}/${pkg.package.package_id}`} className={styles.row}>
                <div className={styles.rowMain}>
                  <span className={styles.rowTitle}>{pkg.title}</span>
                  <Badge tone={state.tone}>{state.label()}</Badge>
                </div>
                <Button
                  size="sm"
                  aria-label={t("settings.cloudSaves.historyFor", { title: pkg.title })}
                  onClick={() => setShown(pkg)}
                >
                  {t("settings.cloudSaves.history")}
                </Button>
              </li>
            );
          })}
        </ul>
      );
  } else if (installs.isError) {
    body = <ErrorState title={t("error.generic")} onRetry={() => void installs.refetch()} />;
  } else body = <LoadingState />;

  return (
    <Section id="saves" title={t("settings.section.saves")} text={t("settings.cloudSaves.text")}>
      {body}
      {shown ? <HistoryDialog pkg={shown} onClose={() => setShown(null)} /> : null}
    </Section>
  );
}

function HistoryDialog({ pkg, onClose }: { pkg: InstalledPackage; onClose: () => void }) {
  const client = useQueryClient();
  const { toast } = useToast();
  const [confirming, setConfirming] = useState<SaveSummary | null>(null);
  const key = ["saves_history", pkg.package.server_id, pkg.package.package_id] as const;
  const history = useQuery({
    queryKey: key,
    queryFn: async () => unwrap(await commands.savesHistory(pkg.package)),
    retry: false,
  });

  const restore = async (entry: SaveSummary) => {
    const result = await commands.savesRestore(pkg.package, entry.source);
    if (result.status === "ok") {
      toast({ tone: "success", title: t("settings.cloudSaves.restored", { title: pkg.title }) });
      setConfirming(null);
      void client.invalidateQueries({ queryKey: key });
      void client.invalidateQueries({ queryKey: queryKeys.installs });
      return;
    }
    setConfirming(null);
    toast({ tone: "danger", title: restoreErrorMessage(result.error) });
  };

  let body: ReactNode;
  if (history.data) {
    const head = history.data.head_snapshot_id;
    body =
      history.data.entries.length === 0 ? (
        <p className={styles.muted}>{t("settings.cloudSaves.noHistory")}</p>
      ) : (
        <ul className={styles.rows} data-nav-group="">
          {history.data.entries.map((entry) => {
            const current = entry.source.kind === "cloud" && entry.source.snapshot_id === head;
            const when = entry.created_at
              ? formatDateTime(entry.created_at)
              : t("settings.cloudSaves.unknownTime");
            return (
              <li key={sourceKey(entry)} className={styles.row}>
                <div className={styles.rowMain}>
                  <span className={styles.rowTitle}>
                    {entry.source.kind === "cloud"
                      ? t("settings.cloudSaves.cloudEntry", { when })
                      : t("settings.cloudSaves.backupEntry", { when })}
                  </span>
                  <span className={styles.muted}>
                    {t("settings.cloudSaves.details", {
                      count: entry.file_count,
                      size: formatBytes(entry.total_bytes),
                      device: entry.device_name ?? t("settings.cloudSaves.thisDevice"),
                    })}
                  </span>
                  {current ? <Badge tone="accent">{t("settings.cloudSaves.current")}</Badge> : null}
                </div>
                {current ? null : (
                  <Button
                    size="sm"
                    aria-label={t("settings.cloudSaves.restoreFrom", { when })}
                    onClick={() => setConfirming(entry)}
                  >
                    {t("settings.cloudSaves.restore")}
                  </Button>
                )}
              </li>
            );
          })}
        </ul>
      );
  } else if (history.isError) {
    body = (
      <ErrorState
        title={t("settings.cloudSaves.historyFailed")}
        onRetry={() => void history.refetch()}
      />
    );
  } else body = <LoadingState />;

  return (
    <>
      <Dialog
        open
        size="lg"
        onClose={onClose}
        title={t("settings.cloudSaves.historyTitle", { title: pkg.title })}
        description={t("settings.cloudSaves.historyText")}
      >
        {body}
      </Dialog>
      <ConfirmDialog
        open={confirming !== null}
        title={t("settings.cloudSaves.confirmTitle")}
        description={t("settings.cloudSaves.confirmText", { title: pkg.title })}
        confirmLabel={t("settings.cloudSaves.restore")}
        onConfirm={() => (confirming ? restore(confirming) : undefined)}
        onCancel={() => setConfirming(null)}
      />
    </>
  );
}

function sourceKey(entry: SaveSummary): string {
  return entry.source.kind === "cloud"
    ? `cloud/${entry.source.snapshot_id}`
    : `backup/${entry.source.backup_id}`;
}

function restoreErrorMessage(error: AppError): string {
  switch (error.kind) {
    case "network":
      return t("settings.cloudSaves.errors.offline");
    case "invalid_input":
      return t("settings.cloudSaves.errors.blocked");
    case "not_found":
      return t("settings.cloudSaves.errors.gone");
    case "unauthenticated":
      return t("settings.cloudSaves.errors.signedOut");
    default:
      return t("settings.cloudSaves.errors.generic");
  }
}
