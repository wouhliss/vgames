// The versions of one game with Withdraw (yank). Nothing is optimistic: the list shows what the
// server confirmed, and a withdrawal needs a reason of 3–500 characters.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { Badge, ErrorState, LoadingState } from "../../components/Feedback";
import { TextArea } from "../../components/TextField";
import { useToast } from "../../components/Toast";
import { formatBytes, formatDateTime, t } from "../../i18n";
import { commands, type PublishPackage, type PublishVersion } from "../../ipc";
import { queryKeys, unwrap } from "../../ipc/query";
import { YANK_REASON_MAX, YANK_REASON_MIN, yankErrorText } from "./model";
import styles from "./Publish.module.css";

export function VersionsPanel({ game }: { game: PublishPackage }) {
  const [yanking, setYanking] = useState<PublishVersion | null>(null);
  const query = useQuery({
    queryKey: queryKeys.publishVersions(game.id),
    queryFn: async () => unwrap(await commands.publishVersions(game.id)),
  });

  let body: React.ReactNode;
  if (query.data) {
    body =
      query.data.length === 0 ? (
        <p className={styles.muted}>{t("publish.versions.none")}</p>
      ) : (
        <ul className={styles.list}>
          {query.data.map((v) => (
            <li key={v.id} className={styles.versionRow}>
              <div>
                <strong>{v.label}</strong>{" "}
                <Badge tone={v.state === "yanked" || v.state === "failed" ? "danger" : "neutral"}>
                  {t(`publish.versions.state.${v.state}`)}
                </Badge>{" "}
                {v.current ? <Badge tone="accent">{t("publish.versions.current")}</Badge> : null}
                <p className={styles.muted}>
                  {formatBytes(v.size_bytes)} · {formatDateTime(v.created_at)}
                </p>
              </div>
              {v.state === "published" ? (
                <Button
                  variant="danger"
                  aria-label={t("publish.versions.yankLabel", { version: v.label })}
                  onClick={() => setYanking(v)}
                >
                  {t("publish.versions.yank")}
                </Button>
              ) : null}
            </li>
          ))}
        </ul>
      );
  } else if (query.isError) {
    body = (
      <ErrorState title={t("publish.versions.loadFailed")} onRetry={() => void query.refetch()} />
    );
  } else body = <LoadingState />;

  return (
    <section aria-labelledby="versions-title" className={styles.step}>
      <h2 id="versions-title">{t("publish.versions.title", { title: game.title })}</h2>
      {body}
      {yanking ? (
        <YankDialog gameId={game.id} version={yanking} onClose={() => setYanking(null)} />
      ) : null}
    </section>
  );
}

function YankDialog({
  gameId,
  version,
  onClose,
}: {
  gameId: string;
  version: PublishVersion;
  onClose: () => void;
}) {
  const client = useQueryClient();
  const { toast } = useToast();
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const length = [...reason.trim()].length;
  const valid = length >= YANK_REASON_MIN && length <= YANK_REASON_MAX;

  const confirm = async () => {
    if (!valid) {
      setError(t("publish.versions.errors.invalidReason"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const result = await commands.publishYank(version.id, reason.trim());
      if (result.status === "error") {
        setError(yankErrorText(result.error));
        return;
      }
      await client.invalidateQueries({ queryKey: queryKeys.publishVersions(gameId) });
      toast({ tone: "success", title: t("publish.versions.yanked", { version: version.label }) });
      onClose();
    } catch {
      setError(t("error.generic"));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open
      title={t("publish.versions.yankTitle", { version: version.label })}
      description={t("publish.versions.yankText")}
      onClose={onClose}
      dismissible={!busy}
      footer={
        <>
          <Button onClick={onClose} aria-disabled={busy || undefined}>
            {t("common.cancel")}
          </Button>
          <Button variant="danger" loading={busy} onClick={() => void confirm()}>
            {t("publish.versions.yankConfirm")}
          </Button>
        </>
      }
    >
      <TextArea
        label={t("publish.versions.yankReason")}
        description={t("publish.versions.yankReasonText")}
        rows={3}
        value={reason}
        error={error}
        onChange={(e) => setReason(e.target.value)}
      />
    </Dialog>
  );
}
