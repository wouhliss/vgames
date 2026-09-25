import { type FormEvent, useEffect, useRef, useState } from "react";
import { Button } from "../../components/Button";
import { Spinner } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { TextField } from "../../components/TextField";
import { t } from "../../i18n";
import { type AuthError, type AuthFlow, commands, events, type ServerProfile } from "../../ipc";
import { useTauriEvent } from "../../ipc/events";
import { authErrorMessage } from "./messages";
import styles from "./Onboarding.module.css";
import { StepHeading } from "./StepHeading";

export function SignInStep({
  server,
  onSignedIn,
}: {
  server: ServerProfile;
  onSignedIn: () => void;
}) {
  const [flow, setFlow] = useState<AuthFlow | null>(null);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<AuthError | null>(null);
  const [pasteOpen, setPasteOpen] = useState(false);
  const [code, setCode] = useState("");
  const [codeError, setCodeError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [browserProblem, setBrowserProblem] = useState(false);
  const flowRef = useRef<AuthFlow | null>(null);
  flowRef.current = flow;
  const codeRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (pasteOpen) codeRef.current?.focus();
  }, [pasteOpen]);

  // Leaving this screen abandons the pending flow.
  useEffect(
    () => () => {
      const pending = flowRef.current;
      if (pending) void commands.authCancel(pending.flow_id).catch(() => {});
    },
    [],
  );

  useTauriEvent(events.authFinished, (payload) => {
    if (payload.flow_id !== flowRef.current?.flow_id) return;
    flowRef.current = null;
    setFlow(null);
    if (payload.outcome.kind === "signed_in") onSignedIn();
    else setError(payload.outcome.error);
  });

  const start = async () => {
    setStarting(true);
    setError(null);
    setCodeError(null);
    const result = await commands.authStart(server.id);
    setStarting(false);
    if (result.status === "error") {
      setError(result.error);
      return;
    }
    setFlow(result.data);
    setBrowserProblem(!result.data.browser_opened);
    setPasteOpen(!result.data.browser_opened);
  };

  const cancel = async () => {
    const pending = flow;
    flowRef.current = null;
    setFlow(null);
    setPasteOpen(false);
    if (pending) await commands.authCancel(pending.flow_id).catch(() => {});
  };

  const submitCode = async (e: FormEvent) => {
    e.preventDefault();
    if (!flow) return;
    if (code.trim() === "") {
      setCodeError(t("onboarding.signIn.errors.emptyCode"));
      return;
    }
    setSubmitting(true);
    const result = await commands.authSubmitCode(flow.flow_id, code.trim());
    setSubmitting(false);
    if (result.status === "ok") {
      flowRef.current = null;
      setFlow(null);
      onSignedIn();
      return;
    }
    if (result.error.kind === "invalid_code") {
      setCodeError(authErrorMessage(result.error));
    } else {
      flowRef.current = null;
      setFlow(null);
      setPasteOpen(false);
      setError(result.error);
    }
  };

  if (!flow) {
    return (
      <section className={styles.card} aria-labelledby="step-title">
        <StepHeading
          title={t("onboarding.signIn.title", { server: server.name })}
          text={t("onboarding.signIn.text")}
        />
        {error ? (
          <Notice tone="danger" role="alert">
            {authErrorMessage(error)}
          </Notice>
        ) : null}
        <div className={styles.actions}>
          <Button
            variant="primary"
            size="lg"
            icon="user"
            loading={starting}
            onClick={start}
            data-autofocus=""
          >
            {t("onboarding.signIn.action")}
          </Button>
        </div>
      </section>
    );
  }

  return (
    <section className={styles.card} aria-labelledby="step-title">
      <StepHeading
        title={t("onboarding.signIn.waitingTitle")}
        text={t("onboarding.signIn.waitingText")}
      />
      {browserProblem ? (
        <Notice tone="warning" role="alert">
          {t("onboarding.signIn.browserNotOpened")}
        </Notice>
      ) : (
        <div className={styles.waiting} role="status">
          <Spinner label={t("onboarding.signIn.waitingTitle")} />
          <span className={styles.muted}>{t("onboarding.signIn.waitingStatus")}</span>
        </div>
      )}
      <div className={`${styles.actions} ${styles.actionsStart}`}>
        <Button
          icon="external"
          onClick={async () => {
            const result = await commands.authOpenBrowser(flow.flow_id);
            if (result.status === "error") {
              if (result.error.kind === "browser_unavailable") {
                setBrowserProblem(true);
                setPasteOpen(true);
              } else {
                await cancel();
                setError(result.error);
              }
            } else setBrowserProblem(false);
          }}
        >
          {t("onboarding.signIn.reopen")}
        </Button>
        {pasteOpen ? null : (
          <Button variant="ghost" onClick={() => setPasteOpen(true)}>
            {t("onboarding.signIn.pasteToggle")}
          </Button>
        )}
      </div>
      {pasteOpen ? (
        <form onSubmit={submitCode} noValidate className={styles.heading}>
          <TextField
            ref={codeRef}
            label={t("onboarding.signIn.codeLabel")}
            description={t("onboarding.signIn.pasteHelp")}
            value={code}
            onChange={(e) => {
              setCode(e.target.value);
              setCodeError(null);
            }}
            error={codeError}
            autoComplete="one-time-code"
            spellCheck={false}
          />
          <div className={styles.actions}>
            <Button type="submit" variant="primary" loading={submitting}>
              {t("onboarding.signIn.submitCode")}
            </Button>
          </div>
        </form>
      ) : null}
      <div className={styles.actions}>
        <Button variant="ghost" onClick={cancel}>
          {t("onboarding.signIn.cancel")}
        </Button>
      </div>
    </section>
  );
}
