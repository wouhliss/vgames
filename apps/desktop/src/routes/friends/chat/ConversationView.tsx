// One conversation: header (who, safety number, invite), the history and the composer. Enter sends,
// Shift+Enter adds a line; messages are 1–4,000 characters. A contact whose key changed pauses
// sending until their new key is trusted from the safety-number screen.
import { type KeyboardEvent, useEffect, useRef, useState } from "react";
import { Button } from "../../../components/Button";
import { ErrorState, LoadingState } from "../../../components/Feedback";
import { Notice } from "../../../components/Notice";
import { TextArea } from "../../../components/TextField";
import { formatNumber, t } from "../../../i18n";
import {
  type Conversation,
  commands,
  type Message,
  type SocialError,
  type UserSummary,
} from "../../../ipc";
import styles from "../Friends.module.css";
import { InviteDialog } from "../InviteDialog";
import { displayName, MESSAGE_MAX, socialErrorText } from "../model";
import { SafetyNumberDialog } from "../SafetyNumberDialog";
import { MessageList } from "./MessageList";
import { useMessages } from "./useMessages";

export function conversationName(conversation: Conversation, myId: string | null): string {
  const others = conversation.members.filter((m) => m.id !== myId);
  return (others.length > 0 ? others : conversation.members).map(displayName).join(", ");
}

export function ConversationView({
  conversation,
  myId,
}: {
  conversation: Conversation;
  myId: string | null;
}) {
  const others = conversation.members.filter((m) => m.id !== myId);
  const name = conversationName(conversation, myId);
  const direct = conversation.kind === "direct" ? (others[0] ?? null) : null;
  const history = useMessages(conversation.id);
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<SocialError | null>(null);
  const [safetyFor, setSafetyFor] = useState<UserSummary | null>(null);
  const [inviting, setInviting] = useState(false);
  const composer = useRef<HTMLTextAreaElement>(null);

  // Opening a conversation reads it.
  useEffect(() => {
    if (history.status === "ready" && conversation.unread > 0)
      void commands.conversationMarkRead(conversation.id);
  }, [history.status, conversation.id, conversation.unread]);

  const length = [...draft].length;
  const trimmed = draft.trimEnd();
  const tooLong = length > MESSAGE_MAX;
  const canSend = trimmed.trim().length > 0 && !tooLong && !sending;

  const send = async () => {
    if (!canSend) return;
    setSending(true);
    setSendError(null);
    try {
      const result = await commands.messageSend(conversation.id, trimmed);
      if (result.status === "error") {
        setSendError(result.error);
        return;
      }
      history.put(result.data);
      setDraft("");
    } catch {
      setSendError({ kind: "internal", detail: "" });
    } finally {
      setSending(false);
      composer.current?.focus();
    }
  };

  const retry = async (message: Message) => {
    const result = await commands.messageRetry(message.id);
    if (result.status === "ok") history.put(result.data, true);
    else setSendError(result.error);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void send();
    }
  };

  const typingNames = [...history.typing]
    .map((id) => conversation.members.find((m) => m.id === id))
    .filter((m): m is UserSummary => m !== undefined)
    .map(displayName);
  const keyChanged = sendError?.kind === "key_changed" ? sendError : null;
  const keyChangedUser = keyChanged
    ? (conversation.members.find((m) => m.id === keyChanged.user_id) ?? null)
    : null;

  return (
    <section className={styles.conversation} aria-labelledby="conversation-title">
      <header className={styles.conversationHeader} data-nav-group="">
        <h2 id="conversation-title">{name}</h2>
        <div className={styles.actions}>
          {direct ? (
            <>
              <Button icon="play" onClick={() => setInviting(true)}>
                {t("friends.inviteToPlay")}
              </Button>
              <Button icon="shield" onClick={() => setSafetyFor(direct)}>
                {t("friends.safetyNumber")}
              </Button>
            </>
          ) : null}
        </div>
      </header>

      {history.status === "loading" ? (
        <LoadingState />
      ) : history.status === "error" ? (
        <ErrorState title={socialErrorText(history.error)} onRetry={() => void history.reload()} />
      ) : history.messages.length === 0 ? (
        <div className={styles.historyEmpty}>
          <p>{t("chat.noMessages")}</p>
          <p className={styles.muted}>{t("chat.noHistoryHelp")}</p>
        </div>
      ) : (
        <MessageList
          label={t("chat.history", { name })}
          messages={history.messages}
          members={conversation.members}
          party={conversation.kind === "party"}
          hasMore={history.hasMore}
          loadingEarlier={history.loadingEarlier}
          earlierFailed={history.earlierFailed}
          onLoadEarlier={history.loadEarlier}
          onRetry={(m) => void retry(m)}
        />
      )}

      <p className={styles.typing} aria-live="polite">
        {typingNames.length > 0 ? t("chat.typing", { name: typingNames.join(", ") }) : ""}
      </p>

      {keyChanged ? (
        <Notice tone="warning" role="alert">
          <p>
            {t("chat.keyChanged", {
              name: keyChangedUser ? displayName(keyChangedUser) : name,
            })}
          </p>
          {keyChangedUser ? (
            <Button size="sm" icon="shield" onClick={() => setSafetyFor(keyChangedUser)}>
              {t("chat.reviewSafety")}
            </Button>
          ) : null}
        </Notice>
      ) : sendError ? (
        <Notice tone="danger" role="alert">
          {t("chat.sendFailed", { detail: socialErrorText(sendError) })}
        </Notice>
      ) : null}

      <form
        className={styles.composer}
        onSubmit={(e) => {
          e.preventDefault();
          void send();
        }}
      >
        <TextArea
          ref={composer}
          label={t("chat.composer", { name })}
          hideLabel
          value={draft}
          rows={2}
          maxChars={MESSAGE_MAX}
          error={tooLong ? t("chat.tooLong", { max: formatNumber(MESSAGE_MAX) }) : null}
          onKeyDown={onKeyDown}
          onChange={(e) => {
            setDraft(e.target.value);
            if (e.target.value.length > 0) void commands.typingStart(conversation.id);
          }}
        />
        <Button
          type="submit"
          variant="primary"
          icon="chat"
          loading={sending}
          aria-disabled={!canSend || undefined}
        >
          {t("chat.send")}
        </Button>
      </form>

      {safetyFor ? (
        <SafetyNumberDialog user={safetyFor} onClose={() => setSafetyFor(null)} />
      ) : null}
      {inviting && direct ? (
        <InviteDialog friend={direct} onClose={() => setInviting(false)} />
      ) : null}
    </section>
  );
}
