// Downloads: what's running (live progress from `install-progress`), what's next (reorderable), and
// what finished. Every action goes through a typed command; the queue refreshes on
// `downloads-changed` and `install-finished`.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { Button } from "../../components/Button";
import { EmptyState, ErrorState, LoadingState } from "../../components/Feedback";
import { useToast } from "../../components/Toast";
import { formatBytes, formatRelativeTime, t } from "../../i18n";
import {
  commands,
  type DownloadActionError,
  type DownloadHistoryEntry,
  type DownloadJob,
  events,
  type InstallProgress,
  type Result,
} from "../../ipc";
import { useTauriEvent } from "../../ipc/events";
import { queryKeys, unwrap } from "../../ipc/query";
import { Page } from "../Page";
import { CancelDialog } from "./CancelDialog";
import { DownloadItem, type ItemActions } from "./DownloadItem";
import styles from "./Downloads.module.css";
import { actionErrorText, moveTo, refKey, waitingJobs } from "./model";

function historyText(entry: DownloadHistoryEntry): string {
  switch (entry.outcome.kind) {
    case "installed":
      return t(`downloads.history.${entry.kind}`, { version: entry.version_label });
    case "cancelled":
      return entry.outcome.kept_partial
        ? t("downloads.history.cancelledKept")
        : t("downloads.history.cancelled");
    case "failed":
      return t("downloads.history.failed", { message: entry.outcome.message });
  }
}

/** Live progress of running jobs; dropped when a job finishes. */
function useLiveProgress(): ReadonlyMap<string, InstallProgress> {
  const [progress, setProgress] = useState<ReadonlyMap<string, InstallProgress>>(() => new Map());
  useTauriEvent(events.installProgress, (p) => {
    setProgress((prev) => new Map(prev).set(refKey(p.package), p));
  });
  useTauriEvent(events.installFinished, (f) => {
    setProgress((prev) => {
      if (!prev.has(refKey(f.package))) return prev;
      const next = new Map(prev);
      next.delete(refKey(f.package));
      return next;
    });
  });
  return progress;
}

export function DownloadsPage() {
  const navigate = useNavigate();
  const client = useQueryClient();
  const { toast } = useToast();
  const downloads = useQuery({
    queryKey: queryKeys.downloads,
    queryFn: async () => unwrap(await commands.downloadsList()),
  });
  const live = useLiveProgress();
  const [busy, setBusy] = useState<ReadonlySet<string>>(() => new Set());
  const [cancelling, setCancelling] = useState<DownloadJob | null>(null);
  const [announcement, setAnnouncement] = useState("");
  // After a reorder, focus returns to the same control of the moved row once the list re-renders.
  const refocus = useRef<{ key: string; action: string } | null>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const jobs = downloads.data?.jobs ?? [];
  const history = downloads.data?.history ?? [];
  const running = jobs.filter((j) => j.state.kind === "active");
  const waiting = useMemo(() => waitingJobs(jobs), [jobs]);

  useEffect(() => {
    const target = refocus.current;
    if (!target || !downloads.data) return;
    refocus.current = null;
    const row = listRef.current?.querySelector<HTMLElement>(
      `[data-job="${CSS.escape(target.key)}"]`,
    );
    const control =
      row?.querySelector<HTMLElement>(`[data-action="${CSS.escape(target.action)}"]`) ??
      row?.querySelector<HTMLElement>("[data-action]");
    control?.focus({ preventScroll: false });
  }, [downloads.data]);

  const run = useCallback(
    async (
      job: DownloadJob,
      command: () => Promise<Result<null, DownloadActionError>>,
    ): Promise<boolean> => {
      const key = refKey(job.package);
      setBusy((prev) => new Set(prev).add(key));
      try {
        const result = await command();
        if (result.status === "error") {
          toast({ tone: "danger", title: actionErrorText(result.error) });
          return false;
        }
        await client.invalidateQueries({ queryKey: queryKeys.downloads });
        return true;
      } catch {
        toast({ tone: "danger", title: t("error.generic") });
        return false;
      } finally {
        setBusy((prev) => {
          const next = new Set(prev);
          next.delete(key);
          return next;
        });
      }
    },
    [client, toast],
  );

  const actions: ItemActions = useMemo(
    () => ({
      busy,
      pause: (job) => void run(job, () => commands.downloadPause(job.package)),
      resume: (job) => void run(job, () => commands.downloadResume(job.package)),
      retry: (job) => void run(job, () => commands.downloadRetry(job.package)),
      remove: (job) => void run(job, () => commands.downloadRemove(job.package)),
      cancel: (job) => setCancelling(job),
      move: (job, index) => {
        const order = moveTo(jobs, job.package, index);
        if (!order) return;
        const position = order.findIndex((r) => refKey(r) === refKey(job.package));
        const focused = document.activeElement?.closest<HTMLElement>("[data-action]");
        refocus.current = {
          key: refKey(job.package),
          action: focused?.dataset.action ?? "more",
        };
        void run(job, () => commands.downloadsReorder(order)).then((ok) => {
          if (ok)
            setAnnouncement(t("downloads.moved", { title: job.title, position: position + 1 }));
        });
      },
    }),
    [busy, jobs, run],
  );

  const confirmCancel = async (keepPartial: boolean) => {
    if (!cancelling) return;
    const ok = await run(cancelling, () =>
      commands.downloadCancel(cancelling.package, keepPartial),
    );
    if (ok) {
      await client.invalidateQueries({ queryKey: queryKeys.installs });
      setCancelling(null);
    }
  };

  const clearHistory = async () => {
    const result = await commands.downloadsHistoryClear().catch(() => null);
    if (result?.status === "ok") {
      await client.invalidateQueries({ queryKey: queryKeys.downloads });
      toast({ tone: "success", title: t("downloads.history.cleared") });
    } else toast({ tone: "danger", title: t("error.generic") });
  };

  let body: ReactNode;
  if (downloads.isPending) body = <LoadingState />;
  else if (downloads.isError && !downloads.data)
    body = <ErrorState title={t("downloads.loadError")} onRetry={() => void downloads.refetch()} />;
  else if (jobs.length === 0 && history.length === 0)
    body = (
      <EmptyState
        icon="download"
        title={t("downloads.emptyTitle")}
        description={t("downloads.emptyText")}
        action={
          <Button variant="primary" icon="browse" onClick={() => navigate("/browse")}>
            {t("downloads.toBrowse")}
          </Button>
        }
      />
    );
  else
    body = (
      <div ref={listRef} className={styles.sections}>
        {jobs.length === 0 ? <p className={styles.muted}>{t("downloads.emptyTitle")}</p> : null}
        {running.length > 0 ? (
          <section aria-labelledby="downloads-running">
            <h2 id="downloads-running" className={styles.sectionTitle}>
              {t("downloads.running")}
            </h2>
            <ul className={styles.list} data-nav-group="">
              {running.map((job) => (
                <DownloadItem
                  key={refKey(job.package)}
                  job={job}
                  live={live.get(refKey(job.package))}
                  position={null}
                  waitingCount={waiting.length}
                  actions={actions}
                />
              ))}
            </ul>
          </section>
        ) : null}
        {waiting.length > 0 ? (
          <section aria-labelledby="downloads-next">
            <div className={styles.sectionHeader}>
              <h2 id="downloads-next" className={styles.sectionTitle}>
                {t("downloads.upNext")}
              </h2>
              {waiting.length > 1 ? (
                <span className={styles.hint}>{t("downloads.reorderHint")}</span>
              ) : null}
            </div>
            <ol className={styles.list} data-nav-group="">
              {waiting.map((job, index) => (
                <DownloadItem
                  key={refKey(job.package)}
                  job={job}
                  live={live.get(refKey(job.package))}
                  position={index}
                  waitingCount={waiting.length}
                  actions={actions}
                />
              ))}
            </ol>
          </section>
        ) : null}
        {history.length > 0 ? (
          <section aria-labelledby="downloads-done">
            <div className={styles.sectionHeader}>
              <h2 id="downloads-done" className={styles.sectionTitle}>
                {t("downloads.completed")}
              </h2>
              <Button
                size="sm"
                variant="ghost"
                aria-label={t("downloads.history.clearTitle")}
                onClick={() => void clearHistory()}
              >
                {t("downloads.history.clear")}
              </Button>
            </div>
            <ul className={styles.history} data-nav-group="">
              {history.map((entry) => (
                <li key={entry.id} className={styles.historyRow} data-outcome={entry.outcome.kind}>
                  <span className={styles.historyTitle}>{entry.title}</span>
                  <span className={styles.historyOutcome}>{historyText(entry)}</span>
                  <span className={styles.historyMeta}>
                    {formatBytes(entry.bytes_total)} · {formatRelativeTime(entry.finished_at)}
                  </span>
                </li>
              ))}
            </ul>
          </section>
        ) : null}
      </div>
    );

  return (
    <Page title={t("downloads.title")}>
      <p className="visually-hidden" role="status" aria-live="polite">
        {announcement}
      </p>
      {body}
      {cancelling ? (
        <CancelDialog
          job={cancelling}
          busy={busy.has(refKey(cancelling.package))}
          onConfirm={(keep) => void confirmCancel(keep)}
          onClose={() => setCancelling(null)}
        />
      ) : null}
    </Page>
  );
}
