// Publish (admins and owners only): pick or create a package, upload a folder as a new version, watch
// it upload and get checked, then release it; withdraw released versions. Non-admins never see the
// route: the sidebar hides it and this page sends them to the library.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useState } from "react";
import { Navigate } from "react-router";
import { useActiveServer } from "../../app/queries";
import { Button } from "../../components/Button";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { ErrorState, LoadingState } from "../../components/Feedback";
import { Select } from "../../components/Select";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import {
  commands,
  events,
  type PublishCommandError,
  type PublishJob,
  type PublishVersion,
  type Result,
} from "../../ipc";
import { useTauriEvent } from "../../ipc/events";
import { commandError, unwrap } from "../../ipc/query";
import { Page } from "../Page";
import { CreatePackageDialog, ResumeKeyDialog, YankDialog } from "./dialogs";
import { JobList } from "./JobList";
import { canPublish, commandErrorText, isRunning, platformLabel } from "./model";
import { NewVersion } from "./NewVersion";
import styles from "./Publish.module.css";
import { Versions } from "./Versions";

export const publishKeys = {
  packages: (serverId: string) => ["publish_packages", serverId] as const,
  versions: (serverId: string, packageId: string) =>
    ["publish_versions", serverId, packageId] as const,
  jobs: ["publish_jobs"] as const,
};

/** A release asked for from a job or from the versions list. */
type Releasing = { versionId: string; title: string; version: string; platform: string };

export function PublishPage() {
  const { server, isPending } = useActiveServer();
  if (isPending) return <LoadingState />;
  if (!server || !canPublish(server.account?.role)) return <Navigate to="/library" replace />;
  return <Publisher serverId={server.id} />;
}

function Publisher({ serverId }: { serverId: string }) {
  const client = useQueryClient();
  const { toast } = useToast();
  const [packageId, setPackageId] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [resuming, setResuming] = useState<PublishJob | null>(null);
  const [releasing, setReleasing] = useState<Releasing | null>(null);
  const [yanking, setYanking] = useState<PublishVersion | null>(null);
  const [busy, setBusy] = useState<ReadonlySet<string>>(() => new Set());

  const packages = useQuery({
    queryKey: publishKeys.packages(serverId),
    queryFn: async () => unwrap(await commands.publishPackages(serverId, null, null)),
  });
  const jobs = useQuery({
    queryKey: publishKeys.jobs,
    queryFn: async () => unwrap(await commands.publishJobs()),
  });
  const pkg = packages.data?.items.find((p) => p.id === packageId) ?? null;
  const versions = useQuery({
    queryKey: publishKeys.versions(serverId, packageId ?? ""),
    queryFn: async () => unwrap(await commands.publishVersions(serverId, packageId ?? "")),
    enabled: packageId !== null,
  });

  const refreshVersions = (pkgId: string) =>
    client.invalidateQueries({ queryKey: publishKeys.versions(serverId, pkgId) });

  useTauriEvent(events.publishProgress, (job) => {
    let ended = false;
    client.setQueryData<PublishJob[]>(publishKeys.jobs, (prev) => {
      const list = prev ?? [];
      const before = list.find((j) => j.id === job.id);
      ended = before !== undefined && isRunning(before) && !isRunning(job);
      return before ? list.map((j) => (j.id === job.id ? job : j)) : [...list, job];
    });
    if (ended || job.phase === "verifying") void refreshVersions(job.package_id);
  });

  const runJob = async (
    job: PublishJob,
    command: () => Promise<Result<unknown, PublishCommandError>>,
  ) => {
    setBusy((prev) => new Set(prev).add(job.id));
    try {
      const result = await command();
      if (result.status === "error") {
        if (result.error.kind === "key_required") setResuming(job);
        else toast({ tone: "danger", title: commandErrorText(result.error) });
      }
      await client.invalidateQueries({ queryKey: publishKeys.jobs });
      await refreshVersions(job.package_id);
    } catch {
      toast({ tone: "danger", title: t("error.generic") });
    } finally {
      setBusy((prev) => {
        const next = new Set(prev);
        next.delete(job.id);
        return next;
      });
    }
  };

  const release = async () => {
    if (!releasing) return;
    const result = await commands.publishRelease(serverId, releasing.versionId).catch(() => null);
    if (result?.status === "ok") {
      toast({
        tone: "success",
        title: t("publish.released", { title: releasing.title, version: releasing.version }),
      });
      setReleasing(null);
      await client.invalidateQueries({ queryKey: publishKeys.jobs });
      await refreshVersions(result.data.package_id);
      await client.invalidateQueries({ queryKey: publishKeys.packages(serverId) });
    } else {
      toast({
        tone: "danger",
        title: result ? commandErrorText(result.error) : t("error.generic"),
      });
    }
  };

  let body: ReactNode;
  if (packages.isPending || jobs.isPending) body = <LoadingState />;
  else if (packages.isError) {
    const error = commandError<PublishCommandError>(packages.error);
    body = (
      <ErrorState
        title={t("publish.loadError")}
        description={error ? commandErrorText(error) : undefined}
        onRetry={() => void packages.refetch()}
      />
    );
  } else {
    const list = jobs.data ?? [];
    body = (
      <div className={styles.sections}>
        <p className={styles.muted}>{t("publish.intro")}</p>
        {list.length > 0 ? (
          <section aria-labelledby="publish-jobs">
            <h2 id="publish-jobs" className={styles.sectionTitle}>
              {t("publish.jobsTitle")}
            </h2>
            <JobList
              jobs={list}
              actions={{
                busy,
                cancel: (job) => void runJob(job, () => commands.publishCancel(job.id)),
                resume: (job) =>
                  job.resume_needs_key
                    ? setResuming(job)
                    : void runJob(job, () => commands.publishResume(job.id, null)),
                release: (job) =>
                  job.version_id
                    ? setReleasing({
                        versionId: job.version_id,
                        title: job.package_title,
                        version: job.version_label,
                        platform: platformLabel(job.platform),
                      })
                    : undefined,
                dismiss: (job) => void runJob(job, () => commands.publishDismiss(job.id)),
              }}
            />
          </section>
        ) : null}

        <section aria-labelledby="publish-new">
          <h2 id="publish-new" className={styles.sectionTitle}>
            {t("publish.newTitle")}
          </h2>
          <div className={styles.form}>
            <div className={styles.row}>
              <Select
                label={t("publish.package")}
                placeholder={t("publish.packagePlaceholder")}
                value={packageId}
                onChange={setPackageId}
                options={(packages.data?.items ?? []).map((p) => ({
                  value: p.id,
                  label: p.title,
                  description: p.slug,
                }))}
              />
              <Button icon="plus" onClick={() => setCreating(true)}>
                {t("publish.newPackage")}
              </Button>
            </div>
          </div>
          {pkg ? (
            <NewVersion
              key={pkg.id}
              serverId={serverId}
              pkg={pkg}
              onStarted={(job) => {
                client.setQueryData<PublishJob[]>(publishKeys.jobs, (prev) => [
                  ...(prev ?? []).filter((j) => j.id !== job.id),
                  job,
                ]);
                void refreshVersions(pkg.id);
              }}
            />
          ) : null}
        </section>

        {pkg ? (
          <section aria-labelledby="publish-versions">
            <h2 id="publish-versions" className={styles.sectionTitle}>
              {t("publish.versionsTitle", { title: pkg.title })}
            </h2>
            {versions.isPending ? (
              <LoadingState />
            ) : versions.isError ? (
              <ErrorState title={t("error.generic")} onRetry={() => void versions.refetch()} />
            ) : (
              <Versions
                versions={versions.data}
                onRelease={(v) =>
                  setReleasing({
                    versionId: v.id,
                    title: pkg.title,
                    version: v.version_label,
                    platform: platformLabel(v.platform),
                  })
                }
                onYank={setYanking}
              />
            )}
          </section>
        ) : null}
      </div>
    );
  }

  return (
    <Page title={t("publish.title")}>
      {body}
      <CreatePackageDialog
        serverId={serverId}
        open={creating}
        onClose={() => setCreating(false)}
        onCreated={(created) => {
          setCreating(false);
          toast({ tone: "success", title: t("publish.created", { title: created.title }) });
          void client
            .invalidateQueries({ queryKey: publishKeys.packages(serverId) })
            .then(() => setPackageId(created.id));
        }}
      />
      <ResumeKeyDialog
        job={resuming}
        onClose={() => setResuming(null)}
        onResumed={() => {
          setResuming(null);
          void client.invalidateQueries({ queryKey: publishKeys.jobs });
        }}
      />
      <ConfirmDialog
        open={releasing !== null}
        title={t("publish.releaseTitle", {
          title: releasing?.title ?? "",
          version: releasing?.version ?? "",
        })}
        description={t("publish.releaseText", { platform: releasing?.platform ?? "" })}
        confirmLabel={t("publish.releaseConfirm")}
        onConfirm={release}
        onCancel={() => setReleasing(null)}
      />
      <YankDialog
        serverId={serverId}
        version={yanking}
        onClose={() => setYanking(null)}
        onYanked={(v) => {
          setYanking(null);
          toast({ tone: "success", title: t("publish.yanked", { version: v.version_label }) });
          void refreshVersions(v.package_id);
        }}
      />
    </Page>
  );
}
