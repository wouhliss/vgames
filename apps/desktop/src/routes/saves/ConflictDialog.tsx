// The cloud-save conflict dialog (06-cloud-saves §3). Both sides changed, so vgames shows them side by
// side and waits: nothing is preselected, Continue stays off until a choice is made, and Cancel (or
// Escape, or B) leaves everything as it is without launching the game.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { ErrorState, LoadingState } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { RadioGroup, type RadioOption } from "../../components/RadioGroup";
import { useToast } from "../../components/Toast";
import { formatBytes, formatDateTime, t } from "../../i18n";
import {
  commands,
  type SaveChoice,
  type SaveConflict,
  type SaveConflictError,
  type SaveSide,
} from "../../ipc";
import { commandError, queryKeys, unwrap } from "../../ipc/query";
import { focusElement } from "../../nav/focus";
import styles from "./Saves.module.css";

export function conflictErrorText(error: SaveConflictError, title: string): string {
  switch (error.kind) {
    case "not_found":
      return t("saves.conflict.notFound");
    case "running":
      return t("saves.conflict.running", { title });
    case "offline":
      return t("saves.conflict.offline");
    case "head_moved":
      return t("saves.conflict.headMoved");
    case "io":
      return t("saves.conflict.failed", { detail: error.detail });
  }
}

function Side({ heading, side }: { heading: string; side: SaveSide }) {
  return (
    <li className={styles.side}>
      <h3>{heading}</h3>
      <p className={styles.when}>
        {t("saves.conflict.changed", { when: formatDateTime(side.changed_at) })}
      </p>
      <p>{t("saves.conflict.from", { device: side.device_name })}</p>
      <p>
        {t("saves.conflict.files", { count: side.file_count })} · {formatBytes(side.size_bytes)}
      </p>
    </li>
  );
}

export function ConflictDialog({
  conflictId,
  onClose,
}: {
  conflictId: string;
  onClose: () => void;
}) {
  const client = useQueryClient();
  const { toast } = useToast();
  const query = useQuery({
    queryKey: queryKeys.saveConflict(conflictId),
    queryFn: async () => unwrap(await commands.savesConflict(conflictId)),
  });
  const [choice, setChoice] = useState<SaveChoice | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // After "head moved" the newer details replace the queried ones.
  const [newer, setNewer] = useState<SaveConflict | null>(null);
  const conflict = newer ?? query.data ?? null;
  const choices = useRef<HTMLDivElement>(null);

  // The dialog opens while the details load, so focus first lands on Cancel. Once there is something
  // to choose, move it to the choices: a keyboard or controller player starts where the decision is.
  const loadedId = conflict?.conflict_id;
  // biome-ignore lint/correctness/useExhaustiveDependencies: only a new conflict moves focus.
  useEffect(() => {
    const first = choices.current?.querySelector<HTMLElement>("[role='radio']");
    if (first) focusElement(first);
  }, [loadedId, newer]);

  // A conflict that no longer exists was resolved elsewhere: there is nothing left to choose.
  const gone = query.isError && commandError<SaveConflictError>(query.error)?.kind === "not_found";

  const apply = async () => {
    if (!conflict || choice === null || busy) return;
    setBusy(true);
    setError(null);
    try {
      const result = await commands.savesResolve(conflict.conflict_id, choice);
      if (result.status === "error") {
        const failure = result.error;
        if (failure.kind === "head_moved") {
          setNewer(failure.conflict);
          setChoice(null);
        }
        if (failure.kind === "not_found") {
          void client.invalidateQueries({ queryKey: queryKeys.installs });
        }
        setError(conflictErrorText(failure, conflict.title));
        return;
      }
      void client.invalidateQueries({ queryKey: queryKeys.installs });
      void client.invalidateQueries({ queryKey: queryKeys.saveHistories });
      toast({
        tone: "success",
        title: t(
          result.data.launched ? "saves.conflict.resolvedLaunched" : "saves.conflict.resolved",
          {
            title: conflict.title,
          },
        ),
      });
      onClose();
    } catch {
      setError(t("error.generic"));
    } finally {
      setBusy(false);
    }
  };

  const options: RadioOption<SaveChoice>[] = [
    {
      value: "keep_cloud",
      label: t("saves.conflict.keepCloud"),
      description: t("saves.conflict.keepCloudText"),
    },
    {
      value: "keep_device",
      label: t("saves.conflict.keepDevice"),
      description: t("saves.conflict.keepDeviceText"),
    },
    {
      value: "keep_both",
      label: t("saves.conflict.keepBoth"),
      description: t("saves.conflict.keepBothText"),
    },
  ];

  let body: React.ReactNode;
  if (conflict) {
    body = (
      <>
        <ul className={styles.sides}>
          <Side heading={t("saves.conflict.thisDevice")} side={conflict.local} />
          <Side heading={t("saves.conflict.cloud")} side={conflict.cloud} />
        </ul>
        <div ref={choices}>
          <RadioGroup
            label={t("saves.conflict.choose")}
            value={choice}
            options={options}
            onChange={setChoice}
          />
        </div>
        <p className={styles.error}>{t("saves.conflict.cancelHint")}</p>
        {error ? (
          <div className={styles.error}>
            <Notice tone={newer ? "warning" : "danger"} role="alert">
              {error}
            </Notice>
          </div>
        ) : null}
      </>
    );
  } else if (gone) {
    body = <Notice tone="info">{t("saves.conflict.notFound")}</Notice>;
  } else if (query.isError) {
    body = (
      <ErrorState title={t("saves.conflict.loadFailed")} onRetry={() => void query.refetch()} />
    );
  } else body = <LoadingState />;

  return (
    <Dialog
      open
      size="lg"
      title={t("saves.conflict.title")}
      description={conflict ? t("saves.conflict.text", { title: conflict.title }) : undefined}
      onClose={onClose}
      dismissible={!busy}
      footer={
        <>
          <Button onClick={onClose} aria-disabled={busy || undefined}>
            {gone ? t("common.close") : t("common.cancel")}
          </Button>
          {conflict ? (
            <Button
              variant="primary"
              loading={busy}
              aria-disabled={choice === null || undefined}
              onClick={() => void apply()}
            >
              {t("saves.conflict.confirm")}
            </Button>
          ) : null}
        </>
      }
    >
      {body}
    </Dialog>
  );
}
