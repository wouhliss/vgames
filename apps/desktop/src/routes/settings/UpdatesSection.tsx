// Updates: the installed version, a manual check, and What's new when an update is known (the same
// dialog as the banner's).
import { useState } from "react";
import { updateVersion } from "../../app/update/model";
import { useUpdaterStatus } from "../../app/update/useUpdater";
import { WhatsNewDialog } from "../../app/update/WhatsNewDialog";
import { Button } from "../../components/Button";
import { t } from "../../i18n";
import { commands, type UpdateCheck } from "../../ipc";
import styles from "./Settings.module.css";
import { Section } from "./SettingsPage";

function checkText(check: UpdateCheck): string {
  switch (check.kind) {
    case "up_to_date":
      return t("settings.updates.upToDate");
    case "available":
      return t("settings.updates.available", { version: check.version });
    case "failed":
      return t("settings.updates.failed", { detail: check.detail });
  }
}

export function UpdatesSection() {
  const status = useUpdaterStatus();
  const version = updateVersion(status.data);
  const [whatsNew, setWhatsNew] = useState(false);
  const [checking, setChecking] = useState(false);
  const [result, setResult] = useState<UpdateCheck | null>(null);

  const check = async () => {
    setChecking(true);
    setResult(null);
    try {
      setResult(await commands.updaterCheck());
    } catch {
      setResult({ kind: "failed", detail: t("error.generic") });
    } finally {
      setChecking(false);
    }
  };

  return (
    <Section id="updates" title={t("settings.section.updates")}>
      {status.data ? (
        <p className={styles.lead}>
          {t("settings.updates.version", { version: status.data.current_version })}
        </p>
      ) : null}
      <div className={styles.actions}>
        <Button icon="refresh" loading={checking} onClick={() => void check()}>
          {checking ? t("settings.updates.checking") : t("settings.updates.check")}
        </Button>
        {version ? (
          <Button onClick={() => setWhatsNew(true)}>
            {t("settings.updates.whatsNew", { version })}
          </Button>
        ) : null}
        <p role="status" className={styles.muted}>
          {result ? checkText(result) : ""}
        </p>
      </div>
      {version ? (
        <WhatsNewDialog
          open={whatsNew}
          onClose={() => setWhatsNew(false)}
          version={version}
          status={status.data}
        />
      ) : null}
    </Section>
  );
}
