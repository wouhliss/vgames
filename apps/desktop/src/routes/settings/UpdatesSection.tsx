// Updates: the installed version and a manual check. (The update banner and What's new come with
// A3-T10, on the same updater commands.)
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Button } from "../../components/Button";
import { t } from "../../i18n";
import { commands, events, type UpdateCheck } from "../../ipc";
import { useTauriEvent } from "../../ipc/events";
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
  const status = useQuery({
    queryKey: ["updater_status"],
    queryFn: () => commands.updaterStatus(),
  });
  useTauriEvent(events.updaterStatus, () => void status.refetch());
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
        <p role="status" className={styles.muted}>
          {result ? checkText(result) : ""}
        </p>
      </div>
    </Section>
  );
}
