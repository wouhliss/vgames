import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Button } from "../../components/Button";
import { EmptyState, ErrorState, LoadingState } from "../../components/Feedback";
import { Switch } from "../../components/Switch";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import { type ConnectedPad, commands, events } from "../../ipc";
import { useTauriEvent } from "../../ipc/events";
import { unwrap } from "../../ipc/query";
import { ControllerDrivers } from "./ControllerDrivers";
import { ControllerProfiles } from "./ControllerProfiles";
import { ControllerTesterDialog } from "./ControllerTesterDialog";
import styles from "./Settings.module.css";
import { Section } from "./SettingsPage";

const PADS = ["controllers_list"] as const;
const SETTINGS = ["controllers_settings_get"] as const;

export function ControllersSection() {
  const client = useQueryClient();
  const { toast } = useToast();
  const [tester, setTester] = useState<ConnectedPad | null>(null);
  const [saving, setSaving] = useState(false);
  const pads = useQuery({
    queryKey: PADS,
    queryFn: async () => unwrap(await commands.controllersList()),
  });
  const settings = useQuery({
    queryKey: SETTINGS,
    queryFn: async () => unwrap(await commands.controllersSettingsGet()),
  });
  useTauriEvent(events.padsChanged, () => {
    void client.invalidateQueries({ queryKey: PADS });
  });

  const save = async (nintendo_labels: boolean) => {
    if (saving) return;
    setSaving(true);
    const result = await commands.controllersSettingsSet({ nintendo_labels }).catch(() => null);
    if (result?.status === "ok") {
      client.setQueryData(SETTINGS, result.data);
      await client.invalidateQueries({ queryKey: ["controllers_profile_get"] });
    } else toast({ tone: "danger", title: t("settings.saveFailed") });
    setSaving(false);
  };
  const liveTester =
    tester && pads.data?.pads.some((pad) => pad.instance_id === tester.instance_id) ? tester : null;

  return (
    <Section id="controllers" title={t("settings.section.controllers")}>
      {settings.data ? (
        <Switch
          label={t("settings.controllers.nintendo")}
          description={t("settings.controllers.nintendoText")}
          checked={settings.data.nintendo_labels}
          disabled={saving}
          onCheckedChange={(value) => void save(value)}
        />
      ) : settings.isError ? (
        <ErrorState title={t("error.generic")} onRetry={() => void settings.refetch()} />
      ) : (
        <LoadingState />
      )}
      <div className={styles.subsection}>
        <h3>{t("settings.controllers.connected")}</h3>
        {pads.data ? (
          pads.data.state.kind === "unavailable" ? (
            <ErrorState
              title={t("settings.controllers.unavailable")}
              description={pads.data.state.detail}
              onRetry={() => void pads.refetch()}
            />
          ) : pads.data.pads.length === 0 ? (
            <EmptyState
              title={t("settings.controllers.empty")}
              description={t("settings.controllers.connect")}
            />
          ) : (
            <ul className={styles.cards}>
              {pads.data.pads.map((pad) => (
                <li key={pad.instance_id} className={styles.card}>
                  <h4>{pad.name}</h4>
                  <p>{t(`settings.controllers.family.${pad.kind}`)}</p>
                  <p>
                    {t(`settings.controllers.connection.${pad.connection}`)} ·{" "}
                    {pad.battery.percent === null
                      ? t("settings.controllers.batteryUnknown")
                      : t("settings.controllers.battery", { percent: pad.battery.percent })}
                  </p>
                  {pad.player_index !== null ? (
                    <p>{t("settings.controllers.player", { player: pad.player_index + 1 })}</p>
                  ) : null}
                  <Button onClick={() => setTester(pad)}>
                    {t("settings.controllers.test", { name: pad.name })}
                  </Button>
                </li>
              ))}
            </ul>
          )
        ) : pads.isError ? (
          <ErrorState title={t("error.generic")} onRetry={() => void pads.refetch()} />
        ) : (
          <LoadingState />
        )}
      </div>
      <ControllerDrivers />
      <ControllerProfiles />
      {liveTester ? (
        <ControllerTesterDialog
          key={liveTester.instance_id}
          pad={liveTester}
          onClose={() => setTester(null)}
        />
      ) : null}
    </Section>
  );
}
