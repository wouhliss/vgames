// The new-version form: folder (scanned, invalid entries listed and blocking), what Play starts,
// platform, version name, key file and passphrase. The key and passphrase go straight to the core.
import { type FormEvent, useState } from "react";
import { Button } from "../../components/Button";
import { Notice } from "../../components/Notice";
import { Select } from "../../components/Select";
import { TextField } from "../../components/TextField";
import { formatBytes, formatNumber, t } from "../../i18n";
import {
  commands,
  type Platform,
  type PublishCommandError,
  type PublishJob,
  type PublishPackage,
  type PublishPlan,
} from "../../ipc";
import { commandErrorText, PLATFORMS, validLabel } from "./model";
import styles from "./Publish.module.css";

const NO_LAUNCH = "";

export function NewVersion({
  serverId,
  pkg,
  onStarted,
}: {
  serverId: string;
  pkg: PublishPackage;
  onStarted: (job: PublishJob) => void;
}) {
  const [plan, setPlan] = useState<PublishPlan | null>(null);
  const [scanning, setScanning] = useState(false);
  const [planError, setPlanError] = useState<PublishCommandError | null>(null);
  const [launch, setLaunch] = useState<string>(NO_LAUNCH);
  const [platform, setPlatform] = useState<Platform | null>(null);
  const [label, setLabel] = useState("");
  const [keyPath, setKeyPath] = useState<string | null>(null);
  const [passphrase, setPassphrase] = useState("");
  const [error, setError] = useState<PublishCommandError | null>(null);
  const [busy, setBusy] = useState(false);

  const chooseFolder = async () => {
    const picked = await commands.publishPickFolder().catch(() => null);
    if (picked?.status !== "ok" || !picked.data) return;
    setScanning(true);
    setPlanError(null);
    const result = await commands.publishPlan(picked.data).catch(() => null);
    setScanning(false);
    if (!result) {
      setPlanError({ kind: "internal", detail: "" });
      return;
    }
    if (result.status === "error") {
      setPlan(null);
      setPlanError(result.error);
      return;
    }
    setPlan(result.data);
    const first = result.data.executables.find((e) => /\.(exe|sh)$/i.test(e));
    setLaunch(first ?? NO_LAUNCH);
  };

  const chooseKey = async () => {
    const picked = await commands.publishPickKey().catch(() => null);
    if (picked?.status === "ok" && picked.data) setKeyPath(picked.data);
  };

  const blocked = plan !== null && plan.invalid_count > 0;
  const ready =
    plan !== null && !blocked && platform !== null && validLabel(label) && keyPath !== null;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!ready || busy || !plan || !platform || !keyPath) return;
    setBusy(true);
    setError(null);
    const result = await commands
      .publishStart(serverId, {
        package_id: pkg.id,
        platform,
        version_label: label.trim(),
        folder: plan.folder,
        launch: launch === NO_LAUNCH ? null : { executable: launch, args: [], working_dir: null },
        key_path: keyPath,
        passphrase,
      })
      .catch(() => null);
    setBusy(false);
    if (!result) {
      setError({ kind: "internal", detail: "" });
      return;
    }
    if (result.status === "error") {
      setError(result.error);
      return;
    }
    setPassphrase("");
    setLabel("");
    onStarted(result.data);
  };

  const keyError = error?.kind === "key" || error?.kind === "key_required";
  const labelError = error?.kind === "invalid_label";

  return (
    <form className={styles.form} onSubmit={(e) => void submit(e)} aria-busy={busy || undefined}>
      <div>
        <p className={styles.path}>{plan?.folder ?? t("publish.noFolder")}</p>
        <Button icon="folder" loading={scanning} onClick={() => void chooseFolder()}>
          {plan ? t("publish.changeFolder") : t("publish.chooseFolder")}
        </Button>
        {scanning ? <p className={styles.muted}>{t("publish.scanning")}</p> : null}
        {planError ? (
          <p className={styles.error} role="alert">
            {commandErrorText(planError)}
          </p>
        ) : null}
        {plan ? (
          <p className={styles.muted}>
            {t("publish.planSummary", {
              files: formatNumber(plan.file_count),
              size: formatBytes(plan.total_bytes),
              packs: formatNumber(plan.pack_count),
            })}
          </p>
        ) : null}
        {blocked ? (
          <Notice tone="danger" role="alert">
            <strong>{t("publish.invalidTitle", { count: plan.invalid_count })}</strong>
            <ul className={styles.invalid}>
              {plan.invalid_paths.map((p) => (
                <li key={p.path}>
                  <span className={styles.path}>{p.path}</span>
                  {" — "}
                  {t(`publish.invalid.${p.reason}`, { other: p.other ?? "" })}
                </li>
              ))}
            </ul>
            {plan.invalid_count > plan.invalid_paths.length ? (
              <p>
                {t("publish.invalidMore", {
                  count: plan.invalid_count - plan.invalid_paths.length,
                })}
              </p>
            ) : null}
          </Notice>
        ) : null}
      </div>

      {plan && !blocked ? (
        <Select
          label={t("publish.launch")}
          value={launch}
          onChange={setLaunch}
          options={[
            { value: NO_LAUNCH, label: t("publish.launchNone") },
            ...plan.executables.map((e) => ({ value: e, label: e })),
          ]}
        />
      ) : null}

      <Select
        label={t("publish.platform")}
        value={platform}
        onChange={setPlatform}
        placeholder={t("publish.platform")}
        options={PLATFORMS}
      />

      <TextField
        label={t("publish.versionLabel")}
        description={t("publish.versionLabelHint")}
        value={label}
        maxLength={64}
        onChange={(e) => setLabel(e.target.value)}
        error={labelError && error ? commandErrorText(error) : null}
      />

      <div>
        <p className={styles.path}>{keyPath ?? t("publish.noKey")}</p>
        <Button icon="lock" onClick={() => void chooseKey()}>
          {t("publish.chooseKey")}
        </Button>
      </div>
      <TextField
        label={t("publish.passphrase")}
        type="password"
        autoComplete="off"
        value={passphrase}
        onChange={(e) => setPassphrase(e.target.value)}
        error={keyError && error ? commandErrorText(error) : null}
      />

      {error && !keyError && !labelError ? (
        <p className={styles.error} role="alert">
          {commandErrorText(error)}
        </p>
      ) : null}
      <div className={styles.actions}>
        <Button
          variant="primary"
          type="submit"
          icon="cloud"
          loading={busy}
          aria-disabled={!ready || undefined}
        >
          {busy ? t("publish.starting") : t("publish.start")}
        </Button>
      </div>
    </form>
  );
}
