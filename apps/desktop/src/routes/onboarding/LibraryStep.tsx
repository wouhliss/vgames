import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { Button } from "../../components/Button";
import { Icon } from "../../components/Icon";
import { formatBytes, t } from "../../i18n";
import { commands, type FolderPick } from "../../ipc";
import { queryKeys } from "../../ipc/query";
import { libraryErrorMessage } from "./messages";
import { Notice } from "./Notice";
import styles from "./Onboarding.module.css";
import { StepHeading } from "./StepHeading";

export function LibraryStep({ onDone }: { onDone: () => void }) {
  const client = useQueryClient();
  const [pick, setPick] = useState<FolderPick | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<"pick" | "add" | null>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);

  // After a folder is chosen, the next action is confirming it.
  useEffect(() => {
    if (pick) confirmRef.current?.focus();
  }, [pick]);

  const choose = async () => {
    setBusy("pick");
    setError(null);
    const result = await commands.libraryPickFolder();
    setBusy(null);
    if (result.status === "error") {
      setError(
        t("onboarding.library.errors.io", {
          detail: "detail" in result.error ? result.error.detail : result.error.kind,
        }),
      );
      return;
    }
    if (result.data) setPick(result.data);
  };

  const add = async () => {
    if (!pick) return;
    setBusy("add");
    const result = await commands.libraryAdd(pick.path, true);
    setBusy(null);
    if (result.status === "error") {
      setError(libraryErrorMessage(result.error));
      return;
    }
    await client.invalidateQueries({ queryKey: queryKeys.libraries });
    onDone();
  };

  return (
    <section className={styles.card} aria-labelledby="step-title">
      <StepHeading title={t("onboarding.library.title")} text={t("onboarding.library.text")} />
      {pick ? (
        <div className={styles.folder}>
          <Icon name="folder" size={28} />
          <div>
            <div className={styles.fingerprintLabel}>{t("onboarding.library.chosen")}</div>
            <div className={styles.folderPath}>{pick.path}</div>
            <div className={styles.muted}>
              {t("onboarding.library.free", {
                free: formatBytes(pick.free_bytes),
                total: formatBytes(pick.total_bytes),
              })}
            </div>
          </div>
        </div>
      ) : null}
      {error ? (
        <Notice tone="danger" role="alert">
          {error}
        </Notice>
      ) : null}
      <div className={styles.actions}>
        <Button
          icon="folder"
          loading={busy === "pick"}
          onClick={choose}
          data-autofocus={pick ? undefined : ""}
        >
          {pick ? t("onboarding.library.change") : t("onboarding.library.pick")}
        </Button>
        {pick ? (
          <Button ref={confirmRef} variant="primary" loading={busy === "add"} onClick={add}>
            {t("onboarding.library.skip")}
          </Button>
        ) : null}
      </div>
    </section>
  );
}
