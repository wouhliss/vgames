// Dialogs of the publish screen: a new package, the key to resume an unsigned upload, withdrawing a
// version. Each shows the core's typed error in place and stays open until it succeeds.
import { type FormEvent, useState } from "react";
import { Button } from "../../components/Button";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { Dialog } from "../../components/Dialog";
import { TextArea, TextField } from "../../components/TextField";
import { t } from "../../i18n";
import {
  commands,
  type PublishCommandError,
  type PublishJob,
  type PublishPackage,
  type PublishVersion,
} from "../../ipc";
import { commandErrorText } from "./model";

function failure(error: unknown): PublishCommandError {
  return { kind: "internal", detail: error instanceof Error ? error.message : String(error) };
}

export function CreatePackageDialog({
  serverId,
  open,
  onClose,
  onCreated,
}: {
  serverId: string;
  open: boolean;
  onClose: () => void;
  onCreated: (pkg: PublishPackage) => void;
}) {
  const [title, setTitle] = useState("");
  const [slug, setSlug] = useState("");
  const [error, setError] = useState<PublishCommandError | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy || !title.trim()) return;
    setBusy(true);
    setError(null);
    const result = await commands
      .publishPackageCreate(serverId, { title: title.trim(), slug: slug.trim() || null })
      .catch((e: unknown) => ({ status: "error", error: failure(e) }) as const);
    setBusy(false);
    if (result.status === "error") {
      setError(result.error);
      return;
    }
    setTitle("");
    setSlug("");
    onCreated(result.data);
  };

  const fieldError = (field: "title" | "slug") =>
    error?.kind === "invalid_field" && error.field === field
      ? error.message
      : field === "slug" && error?.kind === "slug_taken"
        ? commandErrorText(error)
        : null;
  const other =
    error && error.kind !== "invalid_field" && error.kind !== "slug_taken"
      ? commandErrorText(error)
      : null;

  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={t("publish.createTitle")}
      size="sm"
      footer={
        <>
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="primary" type="submit" form="publish-create" loading={busy}>
            {t("publish.create")}
          </Button>
        </>
      }
    >
      <form id="publish-create" onSubmit={(e) => void submit(e)}>
        <TextField
          label={t("publish.createName")}
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          maxLength={200}
          required
          error={fieldError("title")}
        />
        <TextField
          label={t("publish.createSlug")}
          description={t("publish.createSlugHint")}
          value={slug}
          onChange={(e) => setSlug(e.target.value)}
          maxLength={64}
          error={fieldError("slug")}
        />
        {other ? <p role="alert">{other}</p> : null}
      </form>
    </Dialog>
  );
}

/** Resuming an upload that isn't signed yet: the key is needed again. */
export function ResumeKeyDialog({
  job,
  onClose,
  onResumed,
}: {
  job: PublishJob | null;
  onClose: () => void;
  onResumed: () => void;
}) {
  const [keyPath, setKeyPath] = useState<string | null>(null);
  const [passphrase, setPassphrase] = useState("");
  const [error, setError] = useState<PublishCommandError | null>(null);
  const [busy, setBusy] = useState(false);

  const pick = async () => {
    const result = await commands.publishPickKey().catch(() => null);
    if (result?.status === "ok" && result.data) setKeyPath(result.data);
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!job || !keyPath || busy) return;
    setBusy(true);
    setError(null);
    const result = await commands
      .publishResume(job.id, { key_path: keyPath, passphrase })
      .catch((e: unknown) => ({ status: "error", error: failure(e) }) as const);
    setBusy(false);
    if (result.status === "error") {
      setError(result.error);
      return;
    }
    setPassphrase("");
    onResumed();
  };

  return (
    <Dialog
      open={job !== null}
      onClose={onClose}
      title={t("publish.resumeKeyTitle")}
      description={t("publish.resumeKeyText")}
      size="sm"
      footer={
        <>
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button
            variant="primary"
            type="submit"
            form="publish-resume"
            loading={busy}
            aria-disabled={!keyPath || undefined}
          >
            {t("common.continue")}
          </Button>
        </>
      }
    >
      <form id="publish-resume" onSubmit={(e) => void submit(e)}>
        <p>{keyPath ?? t("publish.noKey")}</p>
        <Button icon="lock" onClick={() => void pick()}>
          {t("publish.chooseKey")}
        </Button>
        <TextField
          label={t("publish.passphrase")}
          type="password"
          autoComplete="off"
          value={passphrase}
          onChange={(e) => setPassphrase(e.target.value)}
          error={error ? commandErrorText(error) : null}
        />
      </form>
    </Dialog>
  );
}

export function YankDialog({
  serverId,
  version,
  onClose,
  onYanked,
}: {
  serverId: string;
  version: PublishVersion | null;
  onClose: () => void;
  onYanked: (version: PublishVersion) => void;
}) {
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const valid = reason.trim().length >= 3 && reason.trim().length <= 500;
  if (!version) return null;

  const confirm = async () => {
    const result = await commands
      .versionYank(serverId, version.id, reason.trim())
      .catch((e: unknown) => ({ status: "error", error: failure(e) }) as const);
    if (result.status === "error") {
      setError(commandErrorText(result.error));
      return;
    }
    setReason("");
    setError(null);
    onYanked(result.data);
  };

  return (
    <ConfirmDialog
      open
      tone="danger"
      title={t("publish.yankTitle", { version: version.version_label })}
      description={t("publish.yankText")}
      confirmLabel={t("publish.yankConfirm")}
      confirmDisabled={!valid}
      onConfirm={confirm}
      onCancel={() => {
        setError(null);
        onClose();
      }}
    >
      <TextArea
        label={t("publish.yankReason")}
        description={t("publish.yankReasonHint")}
        value={reason}
        maxChars={500}
        onChange={(e) => setReason(e.target.value)}
        error={error}
      />
    </ConfirmDialog>
  );
}
