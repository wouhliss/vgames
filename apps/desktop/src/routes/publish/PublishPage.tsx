// Publish (admins only, A3-T11): game → folder → version → key → upload, then follow the job. The window
// never reads the folder or the key file: the Rust core scans, unlocks, uploads and signs, and this
// screen shows what it reports. A running or unfinished job replaces the form.
import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useState } from "react";
import { useDebouncedValue } from "../../app/debounce";
import { useActiveServer } from "../../app/queries";
import { Button } from "../../components/Button";
import { ErrorState, LoadingState } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { RadioGroup } from "../../components/RadioGroup";
import { Select } from "../../components/Select";
import { TextArea, TextField } from "../../components/TextField";
import { useToast } from "../../components/Toast";
import { formatBytes, t } from "../../i18n";
import {
  commands,
  events,
  type PickedFolder,
  type PickedKey,
  type Platform,
  type PublishPackage,
  type PublishPlan,
  type PublishProgress,
} from "../../ipc";
import { useTauriEvent } from "../../ipc/events";
import { commandError, queryKeys, unwrap } from "../../ipc/query";
import { Page } from "../Page";
import { JobView } from "./JobView";
import { isValidLabel, jobErrorText, packageErrorText, planErrorText, startError } from "./model";
import styles from "./Publish.module.css";
import { VersionsPanel } from "./VersionsPanel";

type Errors = {
  label?: string | undefined;
  key?: string | undefined;
  passphrase?: string | undefined;
  form?: string | undefined;
};

const PLATFORMS: Platform[] = [
  "windows-x86_64",
  "windows-aarch64",
  "linux-x86_64",
  "linux-aarch64",
  "macos-aarch64",
  "macos-x86_64",
];

export function PublishPage() {
  const { server } = useActiveServer();
  const role = server?.account?.role ?? "user";
  return (
    <Page title={t("publish.title")}>
      {role === "user" ? (
        <Notice tone="warning" role="status">
          {t("publish.notAdmin")}
        </Notice>
      ) : (
        <Publisher />
      )}
    </Page>
  );
}

function Publisher() {
  const client = useQueryClient();
  const { toast } = useToast();
  const [game, setGame] = useState<PublishPackage | null>(null);
  const [search, setSearch] = useState("");
  const query = useDebouncedValue(search, 250);
  const [newTitle, setNewTitle] = useState("");
  const [newError, setNewError] = useState<string | null>(null);
  const [folder, setFolder] = useState<PickedFolder | null>(null);
  const [plan, setPlan] = useState<PublishPlan | null>(null);
  const [planError, setPlanError] = useState<string | null>(null);
  const [scanning, setScanning] = useState(false);
  const [platform, setPlatform] = useState<Platform>("windows-x86_64");
  const [label, setLabel] = useState("");
  const [executable, setExecutable] = useState<string>("");
  const [argsText, setArgsText] = useState("");
  const [key, setKey] = useState<PickedKey | null>(null);
  const [passphrase, setPassphrase] = useState("");
  const [errors, setErrors] = useState<Errors>({});
  const [busy, setBusy] = useState(false);
  // The latest state of every job seen, by id: events can arrive before `publish_start` returns.
  const [jobs, setJobs] = useState<Record<string, PublishProgress>>({});
  const [activeJob, setActiveJob] = useState<string | null>(null);

  useTauriEvent(events.publishProgress, (progress) => {
    setJobs((prev) => ({ ...prev, [progress.job_id]: progress }));
    if (progress.phase === "published") {
      void client.invalidateQueries({ queryKey: queryKeys.publishVersions(progress.package_id) });
    }
  });

  const games = useQuery({
    queryKey: [...queryKeys.publishPackages, query],
    queryFn: async () => unwrap(await commands.publishPackages(query || null)),
    placeholderData: keepPreviousData,
  });
  const unfinished = useQuery({
    queryKey: queryKeys.publishJobs,
    queryFn: async () => unwrap(await commands.publishJobs()),
  });

  const pickFolder = useCallback(async () => {
    setPlanError(null);
    try {
      const picked = unwrap(await commands.publishPickFolder());
      if (!picked) return;
      setFolder(picked);
      setPlan(null);
      setScanning(true);
      const result = await commands.publishPlan(picked.path);
      if (result.status === "error") setPlanError(planErrorText(result.error));
      else {
        setPlan(result.data);
        setExecutable(result.data.executables[0] ?? "");
      }
    } catch {
      setPlanError(t("error.generic"));
    } finally {
      setScanning(false);
    }
  }, []);

  const pickKey = async () => {
    try {
      const picked = unwrap(await commands.publishPickKey());
      if (picked) {
        setKey(picked);
        setErrors((e) => ({ ...e, key: undefined }));
      }
    } catch {
      setErrors((e) => ({ ...e, key: t("error.generic") }));
    }
  };

  const createGame = async () => {
    setNewError(null);
    const result = await commands.publishPackageCreate(newTitle.trim()).catch(() => null);
    if (!result) return setNewError(t("error.generic"));
    if (result.status === "error") return setNewError(packageErrorText(result.error));
    setGame(result.data);
    setNewTitle("");
    await client.invalidateQueries({ queryKey: queryKeys.publishPackages });
    toast({ tone: "success", title: t("publish.package.created", { title: result.data.title }) });
  };

  const blocked = plan !== null && plan.invalid_total > 0;
  const canStart =
    game !== null &&
    plan !== null &&
    !blocked &&
    isValidLabel(label) &&
    key !== null &&
    passphrase !== "";

  const start = async () => {
    if (!game || !plan || !key || busy) return;
    if (!isValidLabel(label)) {
      setErrors({ label: t("publish.release.invalidLabel") });
      return;
    }
    setBusy(true);
    setErrors({});
    try {
      const result = await commands.publishStart({
        plan_id: plan.plan_id,
        package_id: game.id,
        platform,
        version_label: label,
        executable: executable || null,
        arguments: argsText.split("\n").filter((l) => l.trim() !== ""),
        key_id: key.key_id,
        passphrase,
      });
      if (result.status === "error") {
        const e = startError(result.error);
        setErrors(e.field ? { [e.field]: e.text } : { form: e.text });
        if (result.error.kind === "wrong_passphrase") setPassphrase("");
        return;
      }
      // The passphrase is gone from the window as soon as the core has it.
      setPassphrase("");
      setActiveJob(result.data.job_id);
    } catch {
      setErrors({ form: t("error.generic") });
    } finally {
      setBusy(false);
    }
  };

  const reset = () => {
    setActiveJob(null);
    setPlan(null);
    setFolder(null);
    setLabel("");
    setKey(null);
    void client.invalidateQueries({ queryKey: queryKeys.publishJobs });
  };

  const progress = activeJob ? jobs[activeJob] : undefined;
  if (activeJob && progress) {
    const gameFor = game?.id === progress.package_id ? game : null;
    const run = (fn: () => ReturnType<typeof commands.publishCancel>) => async () => {
      const result = await fn().catch(() => null);
      if (!result) return t("error.generic");
      return result.status === "error" ? jobErrorText(result.error) : null;
    };
    return (
      <JobView
        progress={progress}
        gameTitle={gameFor?.title ?? progress.package_id}
        actions={{
          cancel: run(() => commands.publishCancel(activeJob)),
          resume: (pass) => run(() => commands.publishResume(activeJob, pass))(),
          abort: run(() => commands.publishAbort(activeJob)),
          publish: run(() => commands.publishPublish(activeJob)),
          reset,
        }}
      />
    );
  }

  return (
    <div className={styles.page}>
      <p>{t("publish.intro")}</p>

      {unfinished.data && unfinished.data.length > 0 ? (
        <Notice tone="info">
          <ul className={styles.list}>
            {unfinished.data.map((job) => (
              <li key={job.job_id}>
                {t("publish.job.title", { version: job.version_label })} —{" "}
                {t(`publish.job.phase.${job.phase}`)}{" "}
                <Button
                  size="sm"
                  onClick={() => {
                    setJobs((prev) => ({ ...prev, [job.job_id]: job }));
                    setActiveJob(job.job_id);
                  }}
                >
                  {t("publish.job.resume")}
                </Button>
              </li>
            ))}
          </ul>
        </Notice>
      ) : null}

      <section aria-labelledby="step-package" className={styles.step}>
        <h2 id="step-package">{t("publish.steps.package")}</h2>
        <TextField
          label={t("publish.package.search")}
          type="search"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
        {games.data ? (
          games.data.length === 0 ? (
            <p className={styles.muted}>{t("publish.package.none")}</p>
          ) : (
            <RadioGroup
              label={t("publish.package.list")}
              value={game?.id ?? null}
              options={games.data.map((g) => ({
                value: g.id,
                label: g.title,
                description: g.slug,
              }))}
              onChange={(id) => setGame(games.data?.find((g) => g.id === id) ?? null)}
            />
          )
        ) : games.isError ? (
          commandError<{ kind: string }>(games.error)?.kind === "forbidden" ? (
            <Notice tone="warning">{t("publish.notAdmin")}</Notice>
          ) : (
            <ErrorState
              title={t("publish.package.loadFailed")}
              onRetry={() => void games.refetch()}
            />
          )
        ) : (
          <LoadingState />
        )}
        <div className={styles.inline}>
          <TextField
            label={t("publish.package.newTitle")}
            value={newTitle}
            error={newError}
            onChange={(e) => setNewTitle(e.target.value)}
          />
          <Button
            aria-disabled={newTitle.trim() === "" || undefined}
            onClick={() => void createGame()}
          >
            {t("publish.package.create")}
          </Button>
        </div>
      </section>

      <section aria-labelledby="step-folder" className={styles.step}>
        <h2 id="step-folder">{t("publish.steps.folder")}</h2>
        <div>
          <Button onClick={() => void pickFolder()}>
            {folder ? t("publish.folder.chooseAgain") : t("publish.folder.choose")}
          </Button>
        </div>
        {folder ? (
          <p className={styles.muted}>{t("publish.folder.chosen", { path: folder.path })}</p>
        ) : null}
        {scanning ? <LoadingState label={t("publish.folder.scanning")} /> : null}
        {planError ? (
          <Notice tone="danger" role="alert">
            {planError}
          </Notice>
        ) : null}
        {plan ? <PlanPreview plan={plan} /> : null}
      </section>

      <section aria-labelledby="step-release" className={styles.step}>
        <h2 id="step-release">{t("publish.steps.release")}</h2>
        <Select<Platform>
          label={t("publish.release.platform")}
          value={platform}
          options={PLATFORMS.map((p) => ({ value: p, label: t(`platform.${p}`) }))}
          onChange={setPlatform}
        />
        <TextField
          label={t("publish.release.label")}
          description={t("publish.release.labelText")}
          value={label}
          error={errors.label ?? null}
          autoCapitalize="off"
          spellCheck={false}
          onChange={(e) => setLabel(e.target.value)}
        />
        {plan ? (
          <>
            <Select<string>
              label={t("publish.release.program")}
              value={executable}
              options={[
                { value: "", label: t("publish.release.noProgram") },
                ...plan.executables.map((x) => ({ value: x, label: x })),
              ]}
              onChange={setExecutable}
            />
            {executable ? (
              <TextArea
                label={t("publish.release.arguments")}
                rows={2}
                spellCheck={false}
                value={argsText}
                onChange={(e) => setArgsText(e.target.value)}
              />
            ) : null}
          </>
        ) : null}
      </section>

      <section aria-labelledby="step-key" className={styles.step}>
        <h2 id="step-key">{t("publish.steps.key")}</h2>
        <div>
          <Button onClick={() => void pickKey()}>
            {key ? t("publish.key.chooseAgain") : t("publish.key.choose")}
          </Button>
        </div>
        {key ? (
          <p className={styles.muted}>{t("publish.key.chosen", { name: key.file_name })}</p>
        ) : null}
        {errors.key ? (
          <Notice tone="danger" role="alert">
            {errors.key}
          </Notice>
        ) : null}
        <TextField
          label={t("publish.key.passphrase")}
          description={t("publish.key.passphraseText")}
          type="password"
          autoComplete="off"
          value={passphrase}
          error={errors.passphrase ?? null}
          onChange={(e) => setPassphrase(e.target.value)}
        />
      </section>

      {errors.form ? (
        <Notice tone="danger" role="alert">
          {errors.form}
        </Notice>
      ) : null}
      <div>
        <Button
          variant="primary"
          size="lg"
          loading={busy}
          aria-disabled={!canStart || undefined}
          onClick={() => void start()}
        >
          {t("publish.start")}
        </Button>
      </div>

      {game ? <VersionsPanel game={game} /> : null}
    </div>
  );
}

const MAX_SHOWN = 200;

function PlanPreview({ plan }: { plan: PublishPlan }) {
  return (
    <div className={styles.list}>
      <p>
        {t("publish.folder.summary", {
          files: plan.file_count,
          size: formatBytes(plan.total_bytes),
          packs: plan.pack_count,
        })}
      </p>
      {plan.invalid_total > 0 ? (
        <Notice tone="danger" role="alert">
          <strong>{t("publish.folder.invalidTitle", { count: plan.invalid_total })}</strong>
          <p>{t("publish.folder.invalidText")}</p>
          <ul
            className={styles.invalid}
            // biome-ignore lint/a11y/noNoninteractiveTabindex: a long list scrolls, so it must take focus for keyboard and D-pad users.
            tabIndex={0}
            aria-label={t("publish.folder.invalidTitle", { count: plan.invalid_total })}
          >
            {plan.invalid.slice(0, MAX_SHOWN).map((f) => (
              <li key={f.path}>
                <code>{f.path}</code>
                <span>{t(`publish.folder.reasons.${f.reason}`)}</span>
              </li>
            ))}
          </ul>
          {plan.invalid_total > Math.min(plan.invalid.length, MAX_SHOWN) ? (
            <p>
              {t("publish.folder.moreInvalid", {
                count: plan.invalid_total - Math.min(plan.invalid.length, MAX_SHOWN),
              })}
            </p>
          ) : null}
        </Notice>
      ) : (
        <Notice tone="success">{t("publish.folder.ready")}</Notice>
      )}
    </div>
  );
}
