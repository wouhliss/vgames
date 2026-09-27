// General: theme and reduced motion (applied to <html> by AppearanceSync as soon as they change).
import { useQueryClient } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { useAppearanceQuery } from "../../app/appearance";
import { ErrorState, LoadingState } from "../../components/Feedback";
import { RadioGroup } from "../../components/RadioGroup";
import { Switch } from "../../components/Switch";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import { type AppearanceSettings, commands, type Theme } from "../../ipc";
import { queryKeys } from "../../ipc/query";
import { Section } from "./SettingsPage";

const THEMES: Theme[] = ["system", "dark", "light", "high_contrast"];

export function GeneralSection() {
  const appearance = useAppearanceQuery();
  const client = useQueryClient();
  const { toast } = useToast();

  const save = async (next: AppearanceSettings) => {
    const previous = appearance.data;
    client.setQueryData(queryKeys.appearance, next);
    const result = await commands.appearanceSet(next).catch(() => null);
    if (result?.status === "ok") client.setQueryData(queryKeys.appearance, result.data);
    else {
      client.setQueryData(queryKeys.appearance, previous);
      toast({ tone: "danger", title: t("settings.saveFailed") });
    }
  };

  let body: ReactNode;
  if (appearance.data) {
    const current = appearance.data;
    body = (
      <>
        <RadioGroup
          label={t("settings.general.theme")}
          value={current.theme}
          onChange={(theme) => void save({ ...current, theme })}
          options={THEMES.map((theme) => ({
            value: theme,
            label: t(`settings.general.themes.${theme}`),
            description: theme === "system" ? t("settings.general.themeText") : undefined,
          }))}
        />
        <Switch
          label={t("settings.general.reduceMotion")}
          description={t("settings.general.reduceMotionText")}
          checked={current.reduce_motion}
          onCheckedChange={(reduce_motion) => void save({ ...current, reduce_motion })}
        />
      </>
    );
  } else if (appearance.isError) {
    body = <ErrorState title={t("error.generic")} onRetry={() => void appearance.refetch()} />;
  } else body = <LoadingState />;

  return (
    <Section id="general" title={t("settings.section.general")}>
      {body}
    </Section>
  );
}
