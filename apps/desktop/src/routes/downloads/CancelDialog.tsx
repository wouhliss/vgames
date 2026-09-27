// Cancelling a download: keep what was downloaded (continue later from the library) or delete it.
import { useState } from "react";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { RadioGroup } from "../../components/RadioGroup";
import { formatBytes, t } from "../../i18n";
import type { DownloadJob } from "../../ipc";
import styles from "./Downloads.module.css";

type Choice = "keep" | "delete";

export function CancelDialog({
  job,
  busy,
  onConfirm,
  onClose,
}: {
  job: DownloadJob;
  busy: boolean;
  onConfirm: (keepPartial: boolean) => void;
  onClose: () => void;
}) {
  const [choice, setChoice] = useState<Choice>("keep");
  // Nothing downloaded yet: there is nothing to keep, so there is no choice to make.
  const empty = job.bytes_done === 0;
  return (
    <Dialog
      open
      title={t(`downloads.cancelDialog.${job.kind}`, { title: job.title })}
      onClose={onClose}
      dismissible={!busy}
      footer={
        <>
          <Button onClick={onClose} aria-disabled={busy || undefined}>
            {t("downloads.cancelDialog.back")}
          </Button>
          <Button
            variant="danger"
            loading={busy}
            onClick={() => onConfirm(!empty && choice === "keep")}
          >
            {t("downloads.cancelDialog.confirm")}
          </Button>
        </>
      }
    >
      <div className={styles.dialogStack}>
        {job.kind === "install" ? null : <p>{t("downloads.cancelDialog.installedStays")}</p>}
        {empty ? (
          <p>{t("downloads.cancelDialog.nothingYet")}</p>
        ) : (
          <RadioGroup
            label={t("downloads.cancelDialog.keep")}
            hideLabel
            value={choice}
            onChange={setChoice}
            options={[
              {
                value: "keep",
                label: t("downloads.cancelDialog.keep"),
                description: t("downloads.cancelDialog.keepText"),
              },
              {
                value: "delete",
                label: t("downloads.cancelDialog.delete"),
                description: t("downloads.cancelDialog.deleteText", {
                  size: formatBytes(job.bytes_done),
                }),
              },
            ]}
          />
        )}
      </div>
    </Dialog>
  );
}
