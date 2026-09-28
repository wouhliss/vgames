// "Invite to play" (05-social §5), from a friend (pick one of your installed games) or from a package
// page (pick a friend). The optional join info is checked with the launcher's own rule and only ever
// travels end-to-end encrypted; the optional message is plain text, 200 characters at most.
import { useState } from "react";
import { useInstalls } from "../../app/queries";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { LoadingState } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { Select } from "../../components/Select";
import { TextArea, TextField } from "../../components/TextField";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import { commands, type UserSummary } from "../../ipc";
import styles from "./Friends.module.css";
import {
  displayName,
  INVITE_MESSAGE_MAX,
  JOIN_SECRET,
  socialErrorText,
  sortFriends,
} from "./model";
import { useFriends } from "./queries";

export function InviteDialog({
  friend,
  pkg,
  onClose,
}: {
  onClose: () => void;
} & (
  | { friend: UserSummary; pkg?: undefined }
  | { pkg: { id: string; title: string }; friend?: undefined }
)) {
  const { toast } = useToast();
  const friends = useFriends();
  const installs = useInstalls();
  const [friendId, setFriendId] = useState<string | null>(friend?.id ?? null);
  const [packageId, setPackageId] = useState<string | null>(pkg?.id ?? null);
  const [message, setMessage] = useState("");
  const [join, setJoin] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [joinError, setJoinError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const friendOptions = sortFriends(friends.data?.friends ?? []).map((f) => ({
    value: f.user.id,
    label: displayName(f.user),
  }));
  const gameOptions = (installs.data ?? [])
    .filter((i) => i.state === "installed")
    .map((i) => ({ value: i.package.package_id, label: i.title }))
    .sort((a, b) => a.label.localeCompare(b.label));
  const loading = pkg ? friends.isPending : installs.isPending;
  const noChoice = pkg ? friendOptions.length === 0 : gameOptions.length === 0;
  const tooLong = [...message].length > INVITE_MESSAGE_MAX;
  const target = friend ?? friends.data?.friends.find((f) => f.user.id === friendId)?.user ?? null;
  const ready = target !== null && packageId !== null && !tooLong && !noChoice;

  const send = async () => {
    if (!ready || busy) return;
    const secret = join.trim();
    if (secret && !JOIN_SECRET.test(secret)) {
      setJoinError(t("invite.joinInfoInvalid"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const result = await commands.inviteSend(
        target.id,
        packageId,
        message.trim() ? message.trim() : null,
        secret ? secret : null,
      );
      if (result.status === "error") {
        if (result.error.kind === "invalid_input" && result.error.field === "join_secret")
          setJoinError(t("invite.joinInfoInvalid"));
        else setError(socialErrorText(result.error));
        return;
      }
      toast({ tone: "success", title: t("invite.sent", { name: displayName(target) }) });
      onClose();
    } catch {
      setError(socialErrorText(null));
    } finally {
      setBusy(false);
    }
  };

  const title = pkg
    ? `${t("invite.sendTitle")}: ${pkg.title}`
    : `${t("invite.sendTitle")}: ${friend ? displayName(friend) : ""}`;

  return (
    <Dialog
      open
      onClose={onClose}
      dismissible={!busy}
      title={title}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button
            variant="primary"
            icon="play"
            loading={busy}
            aria-disabled={!ready || undefined}
            onClick={() => void send()}
          >
            {t("invite.send")}
          </Button>
        </>
      }
    >
      {loading ? (
        <LoadingState />
      ) : noChoice ? (
        <Notice>{pkg ? t("invite.noFriends") : t("invite.noGames")}</Notice>
      ) : (
        <form
          className={styles.stack}
          onSubmit={(e) => {
            e.preventDefault();
            void send();
          }}
        >
          {error ? (
            <Notice tone="danger" role="alert">
              {error}
            </Notice>
          ) : null}
          {pkg ? (
            <Select
              label={t("invite.friend")}
              value={friendId}
              options={friendOptions}
              onChange={setFriendId}
              placeholder={t("invite.friend")}
            />
          ) : (
            <Select
              label={t("invite.game")}
              value={packageId}
              options={gameOptions}
              onChange={setPackageId}
              placeholder={t("invite.game")}
            />
          )}
          <TextArea
            label={t("invite.message")}
            value={message}
            maxChars={INVITE_MESSAGE_MAX}
            rows={2}
            onChange={(e) => setMessage(e.target.value)}
          />
          <TextField
            label={t("invite.joinInfo")}
            description={t("invite.joinInfoHint")}
            error={joinError}
            value={join}
            autoComplete="off"
            spellCheck={false}
            maxLength={256}
            onChange={(e) => {
              setJoin(e.target.value);
              setJoinError(null);
            }}
          />
        </form>
      )}
    </Dialog>
  );
}
