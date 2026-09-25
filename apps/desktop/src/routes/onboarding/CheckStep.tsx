import { useState } from "react";
import { Button } from "../../components/Button";
import { Fingerprint, Notice } from "../../components/Notice";
import { t } from "../../i18n";
import { commands, type ServerPreview, type ServerProfile } from "../../ipc";
import { serverErrorMessage } from "./messages";
import styles from "./Onboarding.module.css";
import { StepHeading } from "./StepHeading";

export function CheckStep({
  preview,
  onConfirmed,
  onBack,
}: {
  preview: ServerPreview;
  onConfirmed: (server: ServerProfile) => void;
  onBack: (message: string | null) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [mismatch, setMismatch] = useState(false);

  if (mismatch) {
    return (
      <section className={`${styles.card} ${styles.danger}`} aria-labelledby="step-title">
        <StepHeading
          title={t("onboarding.check.mismatchTitle")}
          text={t("onboarding.check.mismatchText")}
        />
        <div className={styles.actions}>
          <Button variant="primary" onClick={() => onBack(null)}>
            {t("onboarding.linkMismatch.action")}
          </Button>
        </div>
      </section>
    );
  }

  const registration =
    preview.registration_mode === "open"
      ? t("onboarding.check.registrationOpen")
      : preview.registration_mode === "allowlist"
        ? t("onboarding.check.registrationAllowlist")
        : preview.registration_mode === "closed"
          ? t("onboarding.check.registrationClosed")
          : null;

  return (
    <section className={styles.card} aria-labelledby="step-title" aria-busy={busy}>
      <StepHeading title={t("onboarding.check.title")} />
      <dl className={styles.meta}>
        <dt>{t("onboarding.check.name")}</dt>
        <dd>
          <strong>{preview.name}</strong>
        </dd>
        <dt>{t("onboarding.check.address")}</dt>
        <dd>{preview.url}</dd>
        {preview.motd ? (
          <>
            <dt>{t("onboarding.check.motd")}</dt>
            <dd>{preview.motd}</dd>
          </>
        ) : null}
      </dl>
      <Fingerprint value={preview.fingerprint} label={t("onboarding.check.fingerprintLabel")} />
      {preview.expected_fingerprint ? (
        <Notice tone="success">{t("onboarding.check.linkMatch")}</Notice>
      ) : (
        <Notice>{t("onboarding.check.explainer")}</Notice>
      )}
      <p className={styles.muted}>{t("onboarding.check.why")}</p>
      {registration ? (
        <Notice tone={preview.registration_mode === "closed" ? "warning" : "info"}>
          {registration}
        </Notice>
      ) : null}
      <p className={styles.muted}>{t("onboarding.check.pinned")}</p>
      <div className={styles.actions}>
        <Button onClick={() => setMismatch(true)}>{t("onboarding.check.mismatch")}</Button>
        <Button
          variant="primary"
          loading={busy}
          onClick={async () => {
            setBusy(true);
            const result = await commands.serverConfirm(preview.preview_id);
            setBusy(false);
            if (result.status === "ok") onConfirmed(result.data);
            else onBack(serverErrorMessage(result.error));
          }}
        >
          {t("onboarding.check.confirm")}
        </Button>
      </div>
    </section>
  );
}
