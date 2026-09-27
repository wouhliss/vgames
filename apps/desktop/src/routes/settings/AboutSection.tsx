// About: version and platform, third-party licenses (plain text from the Rust core), and a
// "copy diagnostics" button for asking for help.
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useAppInfo } from "../../app/queries";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { copyText, ErrorState, LoadingState } from "../../components/Feedback";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import { commands } from "../../ipc";
import { unwrap } from "../../ipc/query";
import styles from "./Settings.module.css";
import { Section } from "./SettingsPage";

function Licenses({ onClose }: { onClose: () => void }) {
  const licenses = useQuery({
    queryKey: ["app_licenses"],
    queryFn: async () => unwrap(await commands.appLicenses()),
  });
  return (
    <Dialog open size="lg" title={t("settings.about.licensesTitle")} onClose={onClose}>
      {licenses.data !== undefined ? (
        // Plain text: never interpreted as markup.
        // biome-ignore lint/a11y/noNoninteractiveTabindex: the scrollable text must be reachable with the keyboard.
        <pre className={styles.licenses} tabIndex={0} data-selectable="">
          {licenses.data}
        </pre>
      ) : licenses.isError ? (
        <ErrorState
          title={t("settings.about.licensesFailed")}
          onRetry={() => void licenses.refetch()}
        />
      ) : (
        <LoadingState />
      )}
    </Dialog>
  );
}

export function AboutSection() {
  const info = useAppInfo();
  const { toast } = useToast();
  const [licenses, setLicenses] = useState(false);

  const copyDiagnostics = async () => {
    const text = await commands.appDiagnostics().catch(() => null);
    if (text !== null && (await copyText(text)))
      toast({ tone: "success", title: t("settings.about.copied") });
    else toast({ tone: "danger", title: t("settings.about.copyFailed") });
  };

  return (
    <Section id="about" title={t("settings.section.about")}>
      {info.data ? (
        <div>
          <p className={styles.lead}>{t("settings.about.title")}</p>
          <p>{t("settings.about.version", { version: info.data.version })}</p>
          <p className={styles.muted}>
            {t("settings.about.build", { os: info.data.os, arch: info.data.arch })}
            {info.data.debug_build ? ` · ${t("settings.about.debug")}` : ""}
          </p>
        </div>
      ) : null}
      <div className={styles.actions}>
        <Button onClick={() => setLicenses(true)}>{t("settings.about.licenses")}</Button>
      </div>
      <div className={styles.subsection}>
        <div className={styles.actions}>
          <Button icon="copy" onClick={() => void copyDiagnostics()}>
            {t("settings.about.diagnostics")}
          </Button>
        </div>
        <p className={styles.muted}>{t("settings.about.diagnosticsText")}</p>
      </div>
      {licenses ? <Licenses onClose={() => setLicenses(false)} /> : null}
    </Section>
  );
}
