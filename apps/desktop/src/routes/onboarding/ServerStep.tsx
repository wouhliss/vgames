import { type FormEvent, useEffect, useRef, useState } from "react";
import { Button } from "../../components/Button";
import { TextField } from "../../components/TextField";
import { t } from "../../i18n";
import { commands, type ServerError, type ServerPreview, type UpdateCheck } from "../../ipc";
import { serverErrorMessage } from "./messages";
import { Notice } from "./Notice";
import styles from "./Onboarding.module.css";
import { StepHeading } from "./StepHeading";

export interface ServerLink {
  url: string;
  fingerprint: string;
}

export function ServerStep({
  adding,
  link,
  initialUrl,
  onPreview,
  onSwitchExisting,
  onCancel,
  onRestart,
}: {
  adding: boolean;
  link: ServerLink | null;
  initialUrl: string;
  onPreview: (preview: ServerPreview) => void;
  onSwitchExisting: (serverId: string) => void;
  onCancel: (() => void) | null;
  onRestart: () => void;
}) {
  const [url, setUrl] = useState(link?.url ?? initialUrl);
  const [error, setError] = useState<ServerError | "empty" | null>(null);
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  const lookUp = async (value: string, expected: string | null) => {
    if (value.trim() === "") {
      setError("empty");
      inputRef.current?.focus();
      return;
    }
    setBusy(true);
    setError(null);
    const result = await commands.serverPreview(value.trim(), expected);
    setBusy(false);
    if (result.status === "ok") onPreview(result.data);
    else {
      setError(result.error);
      inputRef.current?.focus();
    }
  };

  // A vgames://server/add link starts the lookup immediately, pinned to the link's fingerprint.
  const started = useRef(false);
  // biome-ignore lint/correctness/useExhaustiveDependencies: runs once per link.
  useEffect(() => {
    if (!link || started.current) return;
    started.current = true;
    void lookUp(link.url, link.fingerprint);
  }, [link]);

  if (error !== null && error !== "empty" && error.kind === "fingerprint_mismatch") {
    return <LinkMismatch expected={error.expected} actual={error.actual} onRestart={onRestart} />;
  }
  if (error !== null && error !== "empty" && error.kind === "launcher_too_old") {
    return (
      <TooOld
        min={error.min_version}
        current={error.current_version}
        onOther={() => {
          setError(null);
          setUrl("");
        }}
      />
    );
  }

  const message =
    error === "empty" ? t("onboarding.errors.empty") : error ? serverErrorMessage(error) : null;

  const submit = (e: FormEvent) => {
    e.preventDefault();
    void lookUp(url, link?.fingerprint ?? null);
  };

  return (
    <form className={styles.card} onSubmit={submit} noValidate aria-busy={busy}>
      <StepHeading
        title={adding ? t("onboarding.addTitle") : t("onboarding.welcomeTitle")}
        text={adding ? t("onboarding.addText") : t("onboarding.welcomeText")}
      />
      {link ? <Notice tone="warning">{t("onboarding.fromLink")}</Notice> : null}
      <TextField
        ref={inputRef}
        size="lg"
        label={t("onboarding.urlLabel")}
        placeholder={t("onboarding.urlPlaceholder")}
        description={t("onboarding.urlHint")}
        value={url}
        onChange={(e) => {
          setUrl(e.target.value);
          if (error === "empty") setError(null);
        }}
        error={message}
        inputMode="url"
        autoComplete="url"
        autoCapitalize="off"
        spellCheck={false}
        readOnly={busy}
        data-autofocus=""
      />
      {error !== null && error !== "empty" && error.kind === "already_added" ? (
        <div className={`${styles.actions} ${styles.actionsStart}`}>
          <Button onClick={() => onSwitchExisting(error.server_id)}>
            {t("onboarding.errors.switchToIt")}
          </Button>
        </div>
      ) : null}
      <div className={styles.actions}>
        {onCancel ? <Button onClick={onCancel}>{t("onboarding.cancelAdd")}</Button> : null}
        <Button type="submit" variant="primary" loading={busy}>
          {busy ? t("onboarding.checking") : t("common.continue")}
        </Button>
      </div>
    </form>
  );
}

function TooOld({ min, current, onOther }: { min: string; current: string; onOther: () => void }) {
  const [check, setCheck] = useState<UpdateCheck | "checking" | null>(null);
  return (
    <section className={styles.card} aria-labelledby="step-title">
      <StepHeading
        title={t("onboarding.tooOld.title")}
        text={t("onboarding.tooOld.text", { min, current })}
      />
      {check === "checking" ? (
        <Notice role="status">{t("onboarding.tooOld.checking")}</Notice>
      ) : null}
      {check !== null && check !== "checking" ? (
        <Notice
          role="status"
          tone={
            check.kind === "available" ? "success" : check.kind === "failed" ? "danger" : "info"
          }
        >
          {check.kind === "available"
            ? t("onboarding.tooOld.available", { version: check.version })
            : check.kind === "failed"
              ? t("onboarding.tooOld.failed")
              : t("onboarding.tooOld.upToDate")}
        </Notice>
      ) : null}
      <div className={styles.actions}>
        <Button onClick={onOther}>{t("onboarding.tooOld.differentServer")}</Button>
        <Button
          variant="primary"
          icon="refresh"
          loading={check === "checking"}
          onClick={async () => {
            setCheck("checking");
            try {
              setCheck(await commands.updaterCheck());
            } catch {
              setCheck({ kind: "failed", detail: "" });
            }
          }}
        >
          {t("onboarding.tooOld.action")}
        </Button>
      </div>
    </section>
  );
}

function LinkMismatch({
  expected,
  actual,
  onRestart,
}: {
  expected: string;
  actual: string;
  onRestart: () => void;
}) {
  return (
    <section
      className={`${styles.card} ${styles.danger}`}
      aria-labelledby="step-title"
      role="alert"
    >
      <StepHeading
        title={t("onboarding.linkMismatch.title")}
        text={t("onboarding.linkMismatch.text")}
      />
      <dl className={styles.compare}>
        <div>
          <dt>{t("onboarding.linkMismatch.expected")}</dt>
          <dd>{expected}</dd>
        </div>
        <div>
          <dt>{t("onboarding.linkMismatch.actual")}</dt>
          <dd>{actual}</dd>
        </div>
      </dl>
      <p>{t("onboarding.linkMismatch.advice")}</p>
      <div className={styles.actions}>
        <Button variant="primary" onClick={onRestart}>
          {t("onboarding.linkMismatch.action")}
        </Button>
      </div>
    </section>
  );
}
