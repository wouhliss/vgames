// Privacy: whether friends see what you're playing, and do not disturb.
import type { ReactNode } from "react";
import { ErrorState, LoadingState } from "../../components/Feedback";
import { Switch } from "../../components/Switch";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import type { SocialSettings } from "../../ipc";
import { Section } from "./SettingsPage";
import { useSocialSettings } from "./socialSettings";

export function PrivacySection() {
  const { query, save } = useSocialSettings();
  const { toast } = useToast();

  const update = async (next: SocialSettings) => {
    if ((await save(next)) !== null) toast({ tone: "danger", title: t("settings.saveFailed") });
  };

  let body: ReactNode;
  if (query.data) {
    const current = query.data;
    body = (
      <>
        <Switch
          label={t("settings.privacy.showGame")}
          description={t("settings.privacy.showGameText")}
          checked={current.show_current_game}
          onCheckedChange={(show_current_game) => void update({ ...current, show_current_game })}
        />
        <Switch
          label={t("settings.privacy.dnd")}
          description={t("settings.privacy.dndText")}
          checked={current.do_not_disturb}
          onCheckedChange={(do_not_disturb) => void update({ ...current, do_not_disturb })}
        />
      </>
    );
  } else if (query.isError) {
    body = <ErrorState title={t("error.generic")} onRetry={() => void query.refetch()} />;
  } else body = <LoadingState />;

  return (
    <Section id="privacy" title={t("settings.section.privacy")}>
      {body}
    </Section>
  );
}
