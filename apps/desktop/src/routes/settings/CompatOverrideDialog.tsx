// A player's local compatibility override for one game: runner version, graphics backend (macOS) and
// extra environment variables. Saving the defaults removes the override; the Rust core validates
// again (env denylist, runners from the catalog) and its errors show next to the field.
import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { Select } from "../../components/Select";
import { TextArea } from "../../components/TextField";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import {
  type CompatOverride,
  type CompatOverview,
  type CompatSettingsError,
  commands,
  type GraphicsBackend,
  type PackageCompat,
} from "../../ipc";
import {
  COMPAT_PACKAGES,
  findRunner,
  formatEnv,
  isDefaultOverride,
  parseEnv,
  runnerLabel,
  runnerValue,
} from "./compatModel";
import styles from "./Settings.module.css";

type Errors = { runner?: string; graphics?: string; env?: string; form?: string };

function errorsFrom(error: CompatSettingsError | null): Errors {
  if (error === null) return { form: t("settings.saveFailed") };
  switch (error.kind) {
    case "unknown_runner":
      return { runner: t("settings.compat.errors.unknownRunner") };
    case "graphics_unavailable":
      return {
        graphics: t("settings.compat.errors.graphics", {
          backend: t(`settings.compat.graphics.${error.backend}`),
        }),
      };
    case "invalid_env":
      return {
        env:
          error.reason === "denied"
            ? t("settings.compat.errors.envDenied", { key: error.key })
            : t("settings.compat.errors.envFormat", { key: error.key }),
      };
    case "not_found":
      return { form: t("settings.compat.errors.notFound") };
    case "io":
      return { form: t("settings.saveFailed") };
  }
}

export function CompatOverrideDialog({
  pkg,
  overview,
  onClose,
}: {
  pkg: PackageCompat;
  overview: CompatOverview;
  onClose: () => void;
}) {
  const client = useQueryClient();
  const { toast } = useToast();
  const current = pkg.override;
  const [runner, setRunner] = useState(current?.runner ? runnerValue(current.runner) : "default");
  const [graphics, setGraphics] = useState<GraphicsBackend | "auto">(current?.graphics ?? "auto");
  const [envText, setEnvText] = useState(current ? formatEnv(current.env) : "");
  const [errors, setErrors] = useState<Errors>({});
  const [busy, setBusy] = useState(false);

  const defaultLabel = overview.default_runner
    ? t("settings.compat.useDefault", { name: runnerLabel(overview.default_runner) })
    : t("settings.compat.useDefaultAutomatic");

  const save = async () => {
    const env = parseEnv(envText);
    if (!env.ok) {
      setErrors({
        env: t(
          env.problem === "duplicate"
            ? "settings.compat.errors.envDuplicate"
            : "settings.compat.errors.envLine",
          { line: env.line, key: env.key },
        ),
      });
      return;
    }
    const next: CompatOverride = {
      runner: runner === "default" ? null : (findRunner(overview.runners, runner) ?? null),
      graphics: pkg.layer === "wine" && graphics !== "auto" ? graphics : null,
      env: env.env,
    };
    setBusy(true);
    setErrors({});
    let ok = false;
    let error: CompatSettingsError | null = null;
    if (isDefaultOverride(next)) {
      const result = await commands.compatOverrideReset(pkg.package).catch(() => null);
      ok = result?.status === "ok";
    } else {
      const result = await commands.compatOverrideSet(pkg.package, next).catch(() => null);
      ok = result?.status === "ok";
      if (result?.status === "error") error = result.error;
    }
    setBusy(false);
    if (!ok) {
      setErrors(errorsFrom(error));
      return;
    }
    await client.invalidateQueries({ queryKey: COMPAT_PACKAGES });
    toast({ tone: "success", title: t("settings.compat.saved", { title: pkg.title }) });
    onClose();
  };

  return (
    <Dialog
      open
      title={t("settings.compat.dialogTitle", { title: pkg.title })}
      description={t("settings.compat.dialogText")}
      onClose={onClose}
      dismissible={!busy}
      footer={
        <>
          <Button onClick={onClose} aria-disabled={busy || undefined}>
            {t("common.cancel")}
          </Button>
          <Button variant="primary" loading={busy} onClick={() => void save()}>
            {t("settings.compat.save")}
          </Button>
        </>
      }
    >
      <div className={styles.dialogStack}>
        {errors.form ? (
          <p role="alert" className={styles.error}>
            {errors.form}
          </p>
        ) : null}
        <Select
          label={t(`settings.compat.runner.${pkg.layer}`)}
          value={runner}
          error={errors.runner ?? null}
          options={[
            { value: "default", label: defaultLabel },
            ...overview.runners.map((r) => ({ value: runnerValue(r), label: runnerLabel(r) })),
          ]}
          onChange={setRunner}
        />
        {pkg.layer === "wine" ? (
          <Select
            label={t("settings.compat.graphicsLabel")}
            value={graphics}
            error={errors.graphics ?? null}
            options={[
              { value: "auto", label: t("settings.compat.automatic") },
              ...overview.graphics.map((g) => ({
                value: g,
                label: t(`settings.compat.graphics.${g}`),
                description: t(`settings.compat.graphicsText.${g}`),
              })),
            ]}
            onChange={setGraphics}
          />
        ) : null}
        <TextArea
          label={t("settings.compat.env")}
          description={t("settings.compat.envText")}
          value={envText}
          rows={4}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          error={errors.env ?? null}
          onChange={(e) => setEnvText(e.target.value)}
        />
      </div>
    </Dialog>
  );
}
