// One publish job: the phase, upload progress overall and per pack, the server's verification, and what
// can be done next. Follows `publish-progress` events only. A verification or key-trust failure never
// offers to continue; a cancelled or interrupted upload continues from what was already sent.
import { useState } from "react";
import { Button } from "../../components/Button";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { Dialog } from "../../components/Dialog";
import { Notice } from "../../components/Notice";
import { ProgressBar } from "../../components/ProgressBar";
import { TextField } from "../../components/TextField";
import { formatBytes, t } from "../../i18n";
import type { PublishProgress } from "../../ipc";
import { canContinue, failureText, isRunning } from "./model";
import styles from "./Publish.module.css";

export interface JobActions {
  /** Each returns an error sentence, or null when it worked. */
  cancel: () => Promise<string | null>;
  resume: (passphrase: string) => Promise<string | null>;
  abort: () => Promise<string | null>;
  publish: () => Promise<string | null>;
  reset: () => void;
}

type Dialogs = "none" | "cancel" | "resume" | "abort" | "publish";

export function JobView({
  progress,
  gameTitle,
  actions,
}: {
  progress: PublishProgress;
  gameTitle: string;
  actions: JobActions;
}) {
  const [dialog, setDialog] = useState<Dialogs>("none");
  const [error, setError] = useState<string | null>(null);
  const [passphrase, setPassphrase] = useState("");
  const [busy, setBusy] = useState(false);
  const close = () => {
    setDialog("none");
    setPassphrase("");
  };

  const act = async (run: () => Promise<string | null>, onOk?: () => void) => {
    setBusy(true);
    setError(null);
    const problem = await run().catch(() => t("error.generic"));
    setBusy(false);
    if (problem) setError(problem);
    else onOk?.();
    return problem;
  };

  const { phase, failure } = progress;
  const uploading = phase === "uploading" || phase === "preparing";
  const fraction = progress.bytes_total > 0 ? progress.bytes_done / progress.bytes_total : 0;

  return (
    <section aria-labelledby="job-title" className={styles.job}>
      <h2 id="job-title">{t("publish.job.title", { version: progress.version_label })}</h2>
      <p className={styles.phase} role="status">
        {t(`publish.job.phase.${phase}`)}
      </p>

      {phase !== "published" && phase !== "cancelled" ? (
        <ProgressBar
          label={t("publish.job.overall")}
          value={phase === "preparing" ? null : fraction}
          valueText={t("publish.job.bytes", {
            done: formatBytes(progress.bytes_done),
            total: formatBytes(progress.bytes_total),
          })}
        />
      ) : null}
      {uploading && progress.pack_count > 0 ? (
        <>
          <p>{t("publish.job.packs", { done: progress.packs_done, total: progress.pack_count })}</p>
          <ul className={styles.packs}>
            {progress.packs.map((pack) => (
              <li key={pack.index}>
                <ProgressBar
                  label={t("publish.job.pack", { n: pack.index + 1 })}
                  size="sm"
                  value={pack.bytes_total > 0 ? pack.bytes_done / pack.bytes_total : 0}
                />
              </li>
            ))}
          </ul>
        </>
      ) : null}
      {phase === "verifying" ? (
        <ProgressBar label={t("publish.job.verification")} value={progress.verification} />
      ) : null}

      {failure ? (
        <Notice tone="danger" role="alert">
          {failureText(failure)}
        </Notice>
      ) : null}
      {error ? (
        <Notice tone="danger" role="alert">
          {error}
        </Notice>
      ) : null}
      {phase === "published" ? (
        <Notice tone="success" role="status">
          {t("publish.job.publishedText", { version: progress.version_label, game: gameTitle })}
        </Notice>
      ) : null}

      <div className={styles.actions}>
        {isRunning(progress) && phase !== "publishing" && phase !== "verifying" ? (
          <Button onClick={() => setDialog("cancel")}>{t("publish.job.cancel")}</Button>
        ) : null}
        {phase === "ready" ? (
          <Button variant="primary" onClick={() => setDialog("publish")}>
            {t("publish.job.publish")}
          </Button>
        ) : null}
        {phase === "cancelled" || (phase === "failed" && failure && canContinue(failure)) ? (
          <Button variant="primary" onClick={() => setDialog("resume")}>
            {t("publish.job.resume")}
          </Button>
        ) : null}
        {phase === "cancelled" || phase === "failed" || phase === "ready" ? (
          <Button variant="danger" onClick={() => setDialog("abort")}>
            {t("publish.job.abort")}
          </Button>
        ) : null}
        {phase === "published" ? (
          <Button variant="primary" onClick={actions.reset}>
            {t("publish.job.another")}
          </Button>
        ) : null}
      </div>

      <ConfirmDialog
        open={dialog === "cancel"}
        title={t("publish.job.cancelTitle")}
        description={t("publish.job.cancelText")}
        confirmLabel={t("publish.job.cancelConfirm")}
        cancelLabel={t("publish.job.keepGoing")}
        onCancel={close}
        onConfirm={async () => {
          await act(actions.cancel);
          close();
        }}
      />
      <ConfirmDialog
        open={dialog === "abort"}
        tone="danger"
        title={t("publish.job.abortTitle")}
        description={t("publish.job.abortText")}
        confirmLabel={t("publish.job.abort")}
        onCancel={close}
        onConfirm={async () => {
          await act(actions.abort, actions.reset);
          close();
        }}
      />
      <ConfirmDialog
        open={dialog === "publish"}
        title={t("publish.job.publishTitle", { version: progress.version_label })}
        description={t("publish.job.publishText", {
          version: progress.version_label,
          game: gameTitle,
        })}
        confirmLabel={t("publish.job.publishConfirm")}
        onCancel={close}
        onConfirm={async () => {
          await act(actions.publish);
          close();
        }}
      />
      <Dialog
        open={dialog === "resume"}
        title={t("publish.job.resumeTitle")}
        description={t("publish.job.resumeText")}
        onClose={close}
        dismissible={!busy}
        footer={
          <>
            <Button onClick={close}>{t("common.cancel")}</Button>
            <Button
              variant="primary"
              loading={busy}
              aria-disabled={passphrase === "" || undefined}
              onClick={async () => {
                if (passphrase === "") return;
                const problem = await act(() => actions.resume(passphrase));
                if (!problem) close();
              }}
            >
              {t("publish.job.resume")}
            </Button>
          </>
        }
      >
        <TextField
          label={t("publish.key.passphrase")}
          type="password"
          autoComplete="off"
          value={passphrase}
          error={dialog === "resume" ? error : null}
          onChange={(e) => setPassphrase(e.target.value)}
        />
      </Dialog>
    </section>
  );
}
