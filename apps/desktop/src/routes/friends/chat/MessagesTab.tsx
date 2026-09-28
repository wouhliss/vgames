// Messages: conversations on the left (most recent first, unread counts), the open one on the right.
// Each conversation has its own URL (/friends/messages/<id>).
import { NavLink } from "react-router";
import { EmptyState, ErrorState, LoadingState } from "../../../components/Feedback";
import { formatRelativeTime, t } from "../../../i18n";
import type { Conversation, SocialError } from "../../../ipc";
import { commandError } from "../../../ipc/query";
import styles from "../Friends.module.css";
import { displayName, socialErrorText } from "../model";
import { useConversations } from "../queries";
import { ConversationView, conversationName } from "./ConversationView";

function preview(conversation: Conversation, myId: string | null): string {
  const last = conversation.last_message;
  if (!last) return t("chat.noMessages");
  const text =
    last.body.kind === "text"
      ? last.body.text
      : last.body.kind === "invite_join"
        ? t("chat.inviteJoin")
        : last.body.kind === "unsupported"
          ? t("chat.unsupportedPreview")
          : "";
  if (last.mine) return t("chat.you", { text });
  const sender = conversation.members.find((m) => m.id === last.sender_user_id);
  return conversation.kind === "party" && sender && sender.id !== myId
    ? `${displayName(sender)}: ${text}`
    : text;
}

export function MessagesTab({
  conversationId,
  myId,
}: {
  conversationId: string | null;
  myId: string | null;
}) {
  const conversations = useConversations();

  if (conversations.isPending) return <LoadingState />;
  if (conversations.isError)
    return (
      <ErrorState
        title={t("chat.loadFailed")}
        description={socialErrorText(commandError<SocialError>(conversations.error))}
        onRetry={() => void conversations.refetch()}
      />
    );
  if (conversations.data.length === 0)
    return (
      <EmptyState
        icon="chat"
        title={t("chat.empty.title")}
        description={
          <>
            {t("chat.empty.text")} {t("chat.noHistoryHelp")}
          </>
        }
      />
    );

  const open = conversations.data.find((c) => c.id === conversationId) ?? null;
  return (
    <div className={styles.chat}>
      <nav aria-label={t("chat.conversations")} className={styles.conversations} data-nav-group="">
        <ul>
          {conversations.data.map((conversation) => {
            const name = conversationName(conversation, myId);
            return (
              <li key={conversation.id}>
                <NavLink
                  to={`/friends/messages/${conversation.id}`}
                  className={styles.conversationLink ?? ""}
                  aria-label={
                    conversation.unread > 0
                      ? `${name}, ${t("chat.unread", { count: conversation.unread })}`
                      : name
                  }
                >
                  <span className={styles.conversationTop}>
                    <span className={styles.name}>{name}</span>
                    {conversation.last_message ? (
                      <span className={styles.muted}>
                        {formatRelativeTime(conversation.last_message.sent_at)}
                      </span>
                    ) : null}
                  </span>
                  <span className={styles.conversationBottom}>
                    <span className={styles.previewText}>{preview(conversation, myId)}</span>
                    {conversation.unread > 0 ? (
                      <span className={styles.unread} aria-hidden="true">
                        {conversation.unread}
                      </span>
                    ) : null}
                  </span>
                </NavLink>
              </li>
            );
          })}
        </ul>
      </nav>
      {open ? (
        <ConversationView key={open.id} conversation={open} myId={myId} />
      ) : (
        <div className={styles.historyEmpty}>
          <p>{conversationId ? t("friends.error.notFound") : t("chat.pick")}</p>
          {conversationId ? null : <p className={styles.muted}>{t("chat.noHistoryHelp")}</p>}
        </div>
      )}
    </div>
  );
}
