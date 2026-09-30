// The mapping profile of one game, or the default one (07-controllers §5): Nintendo labels, deadzones and
// inverted Y per stick, and button changes. The numbers are checked here, and the Rust core checks
// again; its errors show in the dialog. Mappings are copied and pasted as text, because the window
// never touches files.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { copyText, ErrorState, LoadingState } from "../../components/Feedback";
import { Select } from "../../components/Select";
import { Switch } from "../../components/Switch";
import { TextArea, TextField } from "../../components/TextField";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import {
  type ButtonRemap,
  type ControllerSettingsError,
  commands,
  type PackageControllers,
  type PadButton,
  type PadProfile,
  type StickProfile,
  type XInputButton,
} from "../../ipc";
import { queryKeys, unwrap } from "../../ipc/query";
import {
  padButtonLabel,
  parsePercent,
  profileErrorText,
  unusedButtons,
  X_BUTTONS,
} from "./controllersModel";
import styles from "./Settings.module.css";

/** `null` edits the default profile. */
export function ProfileDialog({
  game,
  onClose,
}: {
  game: PackageControllers | null;
  onClose: () => void;
}) {
  const defaults = useQuery({
    queryKey: queryKeys.controllerDefault,
    queryFn: async () => unwrap(await commands.controllerDefaultProfile()),
  });
  const base = game?.profile ?? defaults.data ?? null;
  const title = game
    ? t("controllers.profile.gameTitle", { title: game.title })
    : t("controllers.profile.defaultTitle");
  return (
    <Dialog
      open
      size="lg"
      title={title}
      description={game ? t("controllers.profile.gameText") : t("controllers.profile.defaultText")}
      onClose={onClose}
    >
      {base ? (
        <Editor game={game} base={base} onClose={onClose} />
      ) : defaults.isError ? (
        <ErrorState title={t("error.generic")} onRetry={() => void defaults.refetch()} />
      ) : (
        <LoadingState />
      )}
    </Dialog>
  );
}

type StickDraft = { deadzone: string; anti: string; invert: boolean };
const draftOf = (s: StickProfile): StickDraft => ({
  deadzone: String(s.deadzone),
  anti: String(s.anti_deadzone),
  invert: s.invert_y,
});

function Editor({
  game,
  base,
  onClose,
}: {
  game: PackageControllers | null;
  base: PadProfile;
  onClose: () => void;
}) {
  const client = useQueryClient();
  const { toast } = useToast();
  const [nintendo, setNintendo] = useState(base.nintendo_labels);
  const [left, setLeft] = useState(draftOf(base.left_stick));
  const [right, setRight] = useState(draftOf(base.right_stick));
  const [remap, setRemap] = useState<ButtonRemap[]>(base.remap);
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [formError, setFormError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [importing, setImporting] = useState(false);
  const [importText, setImportText] = useState("");
  const [importError, setImportError] = useState<string | null>(null);

  const load = (p: PadProfile) => {
    setNintendo(p.nintendo_labels);
    setLeft(draftOf(p.left_stick));
    setRight(draftOf(p.right_stick));
    setRemap(p.remap);
    setFieldErrors({});
    setFormError(null);
  };

  /** The profile the fields describe, or null (with the errors shown) when a number is wrong. */
  const build = (): PadProfile | null => {
    const errors: Record<string, string> = {};
    const stick = (name: string, d: StickDraft): StickProfile => {
      const deadzone = parsePercent(d.deadzone);
      const anti = parsePercent(d.anti);
      if (deadzone === null) errors[`${name}.deadzone`] = t("controllers.profile.errors.number");
      if (anti === null) errors[`${name}.anti`] = t("controllers.profile.errors.number");
      return { deadzone: deadzone ?? 0, anti_deadzone: anti ?? 0, invert_y: d.invert };
    };
    const left_stick = stick("left", left);
    const right_stick = stick("right", right);
    setFieldErrors(errors);
    if (Object.keys(errors).length > 0) return null;
    return { nintendo_labels: nintendo, left_stick, right_stick, remap };
  };

  const save = async () => {
    const profile = build();
    if (!profile || busy) return;
    setBusy(true);
    setFormError(null);
    try {
      const result = await commands.controllerProfileSet(game?.package ?? null, profile);
      if (result.status === "error") {
        setFormError(profileErrorText(result.error));
        return;
      }
      await Promise.all([
        client.invalidateQueries({ queryKey: queryKeys.controllerPackages }),
        client.invalidateQueries({ queryKey: queryKeys.controllerDefault }),
      ]);
      toast({ tone: "success", title: t("controllers.profile.saved") });
      onClose();
    } catch {
      setFormError(t("settings.saveFailed"));
    } finally {
      setBusy(false);
    }
  };

  const reset = async () => {
    setBusy(true);
    try {
      const result = await commands.controllerProfileReset(game?.package ?? null);
      if (result.status === "error") {
        setFormError(t("settings.saveFailed"));
        return;
      }
      await Promise.all([
        client.invalidateQueries({ queryKey: queryKeys.controllerPackages }),
        client.invalidateQueries({ queryKey: queryKeys.controllerDefault }),
      ]);
      toast({ tone: "success", title: t("controllers.profile.saved") });
      onClose();
    } catch {
      setFormError(t("settings.saveFailed"));
    } finally {
      setBusy(false);
    }
  };

  const copy = async () => {
    const profile = build();
    if (!profile) return;
    const text = JSON.stringify(profile, null, 2);
    toast(
      (await copyText(text))
        ? { tone: "success", title: t("controllers.profile.copied") }
        : { tone: "danger", title: t("controllers.profile.copyFailed") },
    );
  };

  const applyImport = async () => {
    setImportError(null);
    try {
      const result = await commands.controllerProfileParse(importText);
      if (result.status === "error") {
        setImportError(profileErrorText(result.error as ControllerSettingsError));
        return;
      }
      load(result.data);
      setImporting(false);
      setImportText("");
      toast({ tone: "info", title: t("controllers.profile.imported") });
    } catch {
      setImportError(t("error.generic"));
    }
  };

  const setRemapAt = (index: number, patch: Partial<ButtonRemap>) =>
    setRemap((r) => r.map((row, i) => (i === index ? { ...row, ...patch } : row)));

  const stickFieldsFor = (name: "left" | "right", d: StickDraft, set: (d: StickDraft) => void) => (
    <fieldset className={styles.fieldset}>
      <legend>
        {t(name === "left" ? "controllers.profile.leftStick" : "controllers.profile.rightStick")}
      </legend>
      <TextField
        label={t("controllers.profile.deadzone")}
        description={t("controllers.profile.deadzoneText")}
        inputMode="numeric"
        value={d.deadzone}
        error={fieldErrors[`${name}.deadzone`] ?? null}
        onChange={(e) => set({ ...d, deadzone: e.target.value })}
      />
      <TextField
        label={t("controllers.profile.antiDeadzone")}
        description={t("controllers.profile.antiDeadzoneText")}
        inputMode="numeric"
        value={d.anti}
        error={fieldErrors[`${name}.anti`] ?? null}
        onChange={(e) => set({ ...d, anti: e.target.value })}
      />
      <Switch
        label={t("controllers.profile.invertY")}
        checked={d.invert}
        onCheckedChange={(invert) => set({ ...d, invert })}
      />
    </fieldset>
  );

  const free = unusedButtons(remap);
  const toOptions = [
    { value: "off", label: t("controllers.profile.off") },
    ...X_BUTTONS.map((x) => ({ value: x as string, label: t(`controllers.xbutton.${x}`) })),
  ];

  return (
    <div className={styles.dialogStack}>
      {formError ? (
        <p role="alert" className={styles.error}>
          {formError}
        </p>
      ) : null}
      <Switch
        label={t("controllers.profile.nintendo")}
        description={t("controllers.profile.nintendoText")}
        checked={nintendo}
        onCheckedChange={setNintendo}
      />
      {stickFieldsFor("left", left, setLeft)}
      {stickFieldsFor("right", right, setRight)}
      <div className={styles.subsection}>
        <h3>{t("controllers.profile.remaps")}</h3>
        <p className={styles.muted}>{t("controllers.profile.remapsText")}</p>
        {remap.length === 0 ? <p>{t("controllers.profile.noRemaps")}</p> : null}
        <ul className={styles.rows} data-nav-group="">
          {remap.map((row, i) => (
            <li key={row.from} className={styles.row}>
              <Select
                label={t("controllers.profile.from")}
                value={row.from}
                options={[row.from, ...free].map((b) => ({
                  value: b,
                  label: padButtonLabel(b),
                }))}
                onChange={(from: PadButton) => setRemapAt(i, { from })}
              />
              <Select
                label={t("controllers.profile.to")}
                value={row.to ?? "off"}
                options={toOptions}
                onChange={(v) => setRemapAt(i, { to: v === "off" ? null : (v as XInputButton) })}
              />
              <Button
                aria-label={t("controllers.profile.removeRemap", {
                  button: padButtonLabel(row.from),
                })}
                onClick={() => setRemap((r) => r.filter((_, j) => j !== i))}
              >
                {t("common.remove")}
              </Button>
            </li>
          ))}
        </ul>
        <div>
          <Button
            aria-disabled={free.length === 0 || undefined}
            onClick={() => {
              const from = free[0];
              if (from) setRemap((r) => [...r, { from, to: null }]);
            }}
          >
            {t("controllers.profile.addRemap")}
          </Button>
        </div>
      </div>
      {importing ? (
        <div className={styles.subsection}>
          <TextArea
            label={t("controllers.profile.importLabel")}
            description={t("controllers.profile.importHint")}
            rows={6}
            spellCheck={false}
            value={importText}
            error={importError}
            onChange={(e) => setImportText(e.target.value)}
          />
          <div className={styles.cardHeader}>
            <Button onClick={() => setImporting(false)}>{t("common.cancel")}</Button>
            <Button variant="primary" onClick={() => void applyImport()}>
              {t("controllers.profile.importApply")}
            </Button>
          </div>
        </div>
      ) : (
        <div className={styles.cardHeader}>
          <Button onClick={() => void copy()}>{t("controllers.profile.copy")}</Button>
          <Button onClick={() => setImporting(true)}>{t("controllers.profile.import")}</Button>
        </div>
      )}
      <div className={styles.cardHeader}>
        <Button onClick={onClose} aria-disabled={busy || undefined}>
          {t("common.cancel")}
        </Button>
        <Button onClick={() => void reset()} aria-disabled={busy || undefined}>
          {game ? t("controllers.profile.resetGame") : t("controllers.profile.reset")}
        </Button>
        <Button variant="primary" loading={busy} onClick={() => void save()}>
          {t("controllers.profile.save")}
        </Button>
      </div>
    </div>
  );
}
