// The cloud-save conflict dialog (06 §3, 06-cloud-saves-notes "Events and launch veto"): the time,
// device and file count of both sides, then Keep cloud / Keep this device / Keep both / Cancel. It
// never picks a side or relaunches the game by itself; Cancel and Escape change nothing.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { Notice } from "../../components/Notice";
import { useToast } from "../../components/Toast";
import { formatBytes, formatDateTime, t } from "../../i18n";
import {
  type AppError,
  type ConflictChoice,
  commands,
  events,
  type SaveConflict,
  type SaveSummary,
} from "../../ipc";
import { useTauriEvent } from "../../ipc/events";
import { commandError, queryKeys, unwrap } from "../../ipc/query";
import { useInstalls } from "../queries";
import { closeSaveConflict, requestSaveConflict, useRequestedSaveConflict } from "./requests";
import styles from "./SaveConflictDialog.module.css";

/** Mounted once in the shell: opens on `save-conflict` and on the launch toast's "Resolve";
 * refreshes the installs list (tile badges) on `save-sync-changed`. */
export function SaveConflictHost() {
  const requested = useRequestedSaveConflict();
  const client = useQueryClient();
  useTauriEvent(events.saveConflict, (conflict) => requestSaveConflict(conflict.conflict_id));
  // Tiles and Settings show each game's sync state from the installs list.
  useTauriEvent(events.saveSyncChanged, () => {
    void client.invalidateQueries({ queryKey: queryKeys.installs });
  });
  if (!requested) return null;
  return <SaveConflictDialog key={requested} conflictId={requested} onClose={closeSaveConflict} />;
}

type Keep = Exclude<ConflictChoice, "cancel">;

function SaveConflictDialog({ conflictId, onClose }: { conflictId: string; onClose: () => void }) {
  const client = useQueryClient();
  const installs = useInstalls();
  const { toast } = useToast();
  const [busy, setBusy] = useState<Keep | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [renewed, setRenewed] = useState(false);
  const conflict = useQuery({
    queryKey: ["save_conflict_get", conflictId],
    queryFn: async () => unwrap(await commands.saveConflictGet(conflictId)),
    retry: false,
  });
  const pkg = conflict.data?.package;
  const title =
    (pkg &&
      installs.data?.find(
        (i) => i.package.server_id === pkg.server_id && i.package.package_id === pkg.package_id,
      )?.title) ??
    t("saves.conflict.thisGame");

  const choose = async (choice: Keep) => {
    setBusy(choice);
    setProblem(null);
    setRenewed(false);
    try {
      const result = await commands.saveConflictResolve(conflictId, choice);
      if (result.status === "error") {
        if (result.error.kind === "not_found") {
          onClose();
          return;
        }
        setProblem(resolveErrorMessage(result.error));
        return;
      }
      void client.invalidateQueries({ queryKey: queryKeys.installs });
      if (result.data.state === "conflict") {
        // The cloud changed while the player was choosing: show the new sides, choose again.
        setRenewed(true);
        await conflict.refetch();
        return;
      }
      toast({ tone: "success", title: t("saves.conflict.done", { title }) });
      onClose();
    } catch {
      setProblem(t("error.generic"));
    } finally {
      setBusy(null);
    }
  };

  const footer = (
    <div className={styles.choices}>
      <Button variant="ghost" disabled={busy !== null} onClick={onClose}>
        {t("saves.conflict.cancel")}
      </Button>
      {conflict.data ? (
        <>
          <Button
            disabled={busy !== null && busy !== "keep_both"}
            loading={busy === "keep_both"}
            onClick={() => void choose("keep_both")}
          >
            {t("saves.conflict.keepBoth")}
          </Button>
          <Button
            disabled={busy !== null && busy !== "keep_device"}
            loading={busy === "keep_device"}
            onClick={() => void choose("keep_device")}
          >
            {t("saves.conflict.keepDevice")}
          </Button>
          <Button
            variant="primary"
            disabled={busy !== null && busy !== "keep_cloud"}
            loading={busy === "keep_cloud"}
            onClick={() => void choose("keep_cloud")}
          >
            {t("saves.conflict.keepCloud")}
          </Button>
        </>
      ) : null}
    </div>
  );

  return (
    <Dialog
      open
      size="lg"
      onClose={onClose}
      dismissible={busy === null}
      title={t("saves.conflict.title", { title })}
      description={t("saves.conflict.description")}
      footer={footer}
    >
      {conflict.isPending ? (
        <p aria-busy="true">{t("saves.conflict.loading")}</p>
      ) : conflict.error ? (
        <Notice tone="danger" role="alert">
          {commandError<AppError>(conflict.error)?.kind === "not_found"
            ? t("saves.conflict.gone")
            : t("saves.conflict.loadFailed")}
        </Notice>
      ) : (
        <>
          {renewed ? (
            <Notice tone="warning" role="status">
              {t("saves.conflict.renewed")}
            </Notice>
          ) : null}
          {problem ? (
            <Notice tone="danger" role="alert">
              {problem}
            </Notice>
          ) : null}
          <Sides conflict={conflict.data} />
        </>
      )}
    </Dialog>
  );
}

function Sides({ conflict }: { conflict: SaveConflict }) {
  return (
    <div className={styles.sides}>
      <Side
        heading={t("saves.conflict.cloud")}
        summary={conflict.cloud}
        device={conflict.cloud.device_name ?? t("saves.conflict.otherDevice")}
      />
      <Side
        heading={t("saves.conflict.device")}
        summary={conflict.device}
        device={t("saves.conflict.thisDevice")}
      />
    </div>
  );
}

function Side({
  heading,
  summary,
  device,
}: {
  heading: string;
  summary: SaveSummary;
  device: string;
}) {
  return (
    <section className={styles.side} aria-label={heading}>
      <h3>{heading}</h3>
      <dl>
        <dt>{t("saves.conflict.savedAt")}</dt>
        <dd>
          {summary.created_at ? formatDateTime(summary.created_at) : t("saves.conflict.unknown")}
        </dd>
        <dt>{t("saves.conflict.from")}</dt>
        <dd>{device}</dd>
        <dt>{t("saves.conflict.files")}</dt>
        <dd>
          {t("saves.conflict.fileCount", {
            count: summary.file_count,
            size: formatBytes(summary.total_bytes),
          })}
        </dd>
      </dl>
    </section>
  );
}

function resolveErrorMessage(error: AppError): string {
  switch (error.kind) {
    case "network":
      return t("saves.conflict.errors.offline");
    case "server":
      return error.code === "save_quota_exceeded"
        ? t("saves.conflict.errors.quota")
        : t("saves.conflict.errors.server");
    case "invalid_input":
      return t("saves.conflict.errors.running");
    case "unauthenticated":
      return t("saves.conflict.errors.signedOut");
    default:
      return t("saves.conflict.errors.generic");
  }
}
