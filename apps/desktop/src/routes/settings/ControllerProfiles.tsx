import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useId, useState } from "react";
import { Button } from "../../components/Button";
import { EmptyState, ErrorState, LoadingState } from "../../components/Feedback";
import { RadioGroup } from "../../components/RadioGroup";
import { Select } from "../../components/Select";
import { Switch } from "../../components/Switch";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import {
  type ControllerButton,
  type ControllerProfile,
  type ControllerStickSettings,
  commands,
  type PackageRef,
  type XboxButton,
} from "../../ipc";
import { queryKeys, unwrap } from "../../ipc/query";
import styles from "./Settings.module.css";

const BUTTONS: readonly ControllerButton[] = [
  "south",
  "east",
  "west",
  "north",
  "back",
  "guide",
  "start",
  "left_stick",
  "right_stick",
  "left_shoulder",
  "right_shoulder",
  "dpad_up",
  "dpad_down",
  "dpad_left",
  "dpad_right",
  "touchpad",
];
const TARGETS: readonly XboxButton[] = [
  "a",
  "b",
  "x",
  "y",
  "back",
  "guide",
  "start",
  "left_thumb",
  "right_thumb",
  "left_shoulder",
  "right_shoulder",
  "dpad_up",
  "dpad_down",
  "dpad_left",
  "dpad_right",
];
type RemapTarget = XboxButton | "default" | "disabled";
const key = (pkg: PackageRef) => `${pkg.server_id}/${pkg.package_id}`;
const profileKey = (pkg: PackageRef) =>
  ["controllers_profile_get", pkg.server_id, pkg.package_id] as const;

function PercentControl({
  label,
  value,
  onChange,
  disabled,
}: {
  label: string;
  value: number;
  onChange: (value: number) => void;
  disabled: boolean;
}) {
  const id = useId();
  return (
    <div>
      <label htmlFor={id}>{label}</label>
      <input
        id={id}
        type="range"
        min={0}
        max={99}
        step={1}
        value={value}
        disabled={disabled}
        aria-valuetext={`${value}%`}
        onChange={(event) => onChange(Number(event.target.value))}
        onKeyDown={(event) => {
          // Synthetic gamepad keys do not perform the browser's default range
          // action, so handle left/right here before spatial navigation sees it.
          if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
          event.preventDefault();
          onChange(Math.max(0, Math.min(99, value + (event.key === "ArrowRight" ? 1 : -1))));
        }}
      />
      <output htmlFor={id}>{value}%</output>
    </div>
  );
}

function Editor({ pkg }: { pkg: PackageRef }) {
  const query = useQuery({
    queryKey: profileKey(pkg),
    queryFn: async () => unwrap(await commands.controllersProfileGet(pkg)),
  });
  if (query.data) return <ProfileForm pkg={pkg} initial={query.data} />;
  if (query.isError)
    return <ErrorState title={t("error.generic")} onRetry={() => void query.refetch()} />;
  return <LoadingState />;
}

function ProfileForm({ pkg, initial }: { pkg: PackageRef; initial: ControllerProfile }) {
  const [draft, setDraft] = useState(initial);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const client = useQueryClient();
  const { toast } = useToast();
  useEffect(() => {
    setDraft(initial);
    setError(null);
  }, [initial]);
  const stick = <Field extends keyof ControllerStickSettings>(
    side: "left_stick" | "right_stick",
    field: Field,
    value: ControllerStickSettings[Field],
  ) => {
    setDraft((current) => ({
      ...current,
      mapping: { ...current.mapping, [side]: { ...current.mapping[side], [field]: value } },
    }));
  };
  const remap = (from: ControllerButton, to: RemapTarget) => {
    setDraft((current) => {
      const next = current.mapping.remap.filter((item) => item.from !== from);
      if (to !== "default") next.push({ from, to: to === "disabled" ? null : to });
      return { ...current, mapping: { ...current.mapping, remap: next } };
    });
  };
  const save = async () => {
    if (saving) return;
    setSaving(true);
    setError(null);
    const result = await commands.controllersProfileSet(pkg, draft).catch(() => null);
    if (result?.status === "ok") {
      client.setQueryData(profileKey(pkg), result.data);
      toast({ tone: "success", title: t("settings.saved") });
    } else
      setError(
        result?.status === "error" && result.error.kind === "invalid_input"
          ? result.error.message
          : t("settings.saveFailed"),
      );
    setSaving(false);
  };
  return (
    <div className={styles.subsection} data-nav-group="">
      <RadioGroup
        label={t("settings.controllers.profiles.mode")}
        value={draft.mode}
        options={(["auto", "always", "never"] as const).map((value) => ({
          value,
          label: t(`settings.controllers.profiles.modes.${value}`),
          disabled: saving,
        }))}
        onChange={(mode) => setDraft((current) => ({ ...current, mode }))}
      />
      <Switch
        label={t("settings.controllers.profiles.nintendo")}
        checked={draft.mapping.nintendo_labels}
        disabled={saving}
        onCheckedChange={(nintendo_labels) =>
          setDraft((current) => ({ ...current, mapping: { ...current.mapping, nintendo_labels } }))
        }
      />
      {(["left_stick", "right_stick"] as const).map((side) => (
        <fieldset key={side} className={styles.subsection}>
          <legend>{t(`settings.controllers.profiles.${side}`)}</legend>
          <PercentControl
            label={t(`settings.controllers.profiles.${side}Deadzone`)}
            value={draft.mapping[side].deadzone}
            disabled={saving}
            onChange={(value) => stick(side, "deadzone", value)}
          />
          <PercentControl
            label={t(`settings.controllers.profiles.${side}AntiDeadzone`)}
            value={draft.mapping[side].anti_deadzone}
            disabled={saving}
            onChange={(value) => stick(side, "anti_deadzone", value)}
          />
          <Switch
            label={t(`settings.controllers.profiles.${side}Invert`)}
            checked={draft.mapping[side].invert_y}
            disabled={saving}
            onCheckedChange={(value) => stick(side, "invert_y", value)}
          />
        </fieldset>
      ))}
      <h4>{t("settings.controllers.profiles.remaps")}</h4>
      {BUTTONS.map((from) => {
        const override = draft.mapping.remap.find((item) => item.from === from);
        const value: RemapTarget = override ? (override.to ?? "disabled") : "default";
        return (
          <Select<RemapTarget>
            key={from}
            label={t("settings.controllers.profiles.remap", {
              button: t(`settings.controllers.buttonNames.${from}`),
            })}
            value={value}
            disabled={saving}
            onChange={(to) => remap(from, to)}
            options={[
              { value: "default", label: t("settings.controllers.profiles.default") },
              { value: "disabled", label: t("settings.controllers.profiles.disabled") },
              ...TARGETS.map((value) => ({
                value,
                label: t(`settings.controllers.profiles.targets.${value}`),
              })),
            ]}
          />
        );
      })}
      {error ? (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      ) : null}
      <Button variant="primary" loading={saving} onClick={() => void save()}>
        {t("settings.controllers.profiles.save")}
      </Button>
    </div>
  );
}

export function ControllerProfiles() {
  const [selected, setSelected] = useState<string | null>(null);
  const games = useQuery({
    queryKey: queryKeys.installs,
    queryFn: async () => unwrap(await commands.installsList()),
  });
  const servers = useQuery({
    queryKey: queryKeys.servers,
    queryFn: () => commands.serversList(),
  });
  const pkg = games.data?.find((game) => key(game.package) === selected)?.package;
  return (
    <div className={styles.subsection}>
      <h3>{t("settings.controllers.profiles.title")}</h3>
      <p>{t("settings.controllers.profiles.text")}</p>
      {games.data && servers.data ? (
        games.data.length ? (
          <>
            <Select
              label={t("settings.controllers.profiles.game")}
              value={selected}
              onChange={setSelected}
              options={games.data.map((game) => ({
                value: key(game.package),
                label: `${game.title} · ${servers.data?.find((server) => server.id === game.package.server_id)?.name ?? t("settings.controllers.profiles.unknownServer")}`,
              }))}
            />
            {pkg ? <Editor key={key(pkg)} pkg={pkg} /> : null}
          </>
        ) : (
          <EmptyState title={t("settings.controllers.profiles.empty")} />
        )
      ) : games.isError || servers.isError ? (
        <ErrorState
          title={t("error.generic")}
          onRetry={() => {
            void games.refetch();
            void servers.refetch();
          }}
        />
      ) : (
        <LoadingState />
      )}
    </div>
  );
}
