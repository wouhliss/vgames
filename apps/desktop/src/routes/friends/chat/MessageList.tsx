// The virtualized message history (only the rows near the viewport are in the DOM). It stays pinned
// to the newest message while you're at the bottom, keeps its place when earlier messages load, and
// is a focusable log so the keyboard and the D-pad can scroll it.
import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useLayoutEffect, useRef } from "react";
import { Button } from "../../../components/Button";
import { formatDateTime, t } from "../../../i18n";
import type { Message, UserSummary } from "../../../ipc";
import styles from "../Friends.module.css";
import { displayName } from "../model";

function noticeText(message: Message, members: readonly UserSummary[]): string | null {
  if (message.body.kind !== "notice") return null;
  const notice = message.body.notice;
  const user = members.find((m) => m.id === notice.user_id);
  const name = user ? displayName(user) : t("friends.blockedList.unknown");
  switch (notice.kind) {
    case "new_device":
      return t("chat.notice.newDevice", { name, device: notice.device_name });
    case "key_changed":
      return t("chat.notice.keyChanged", { name });
    case "device_revoked":
      return t("chat.notice.deviceRevoked", { name });
  }
}

function MessageBodyView({ message }: { message: Message }) {
  switch (message.body.kind) {
    case "text":
      // Plain text only: never interpreted as HTML or Markdown.
      return <p className={styles.messageText}>{message.body.text}</p>;
    case "invite_join":
      return <p className={styles.messageMeta}>{t("chat.inviteJoin")}</p>;
    case "unsupported":
      return <p className={styles.messageMeta}>{t("chat.unsupported")}</p>;
    case "notice":
      return null;
  }
}

function MessageRow({
  message,
  members,
  showSender,
  onRetry,
}: {
  message: Message;
  members: readonly UserSummary[];
  showSender: boolean;
  onRetry: (message: Message) => void;
}) {
  const notice = noticeText(message, members);
  if (notice !== null) {
    return (
      <div className={styles.notice} data-kind={message.body.kind}>
        {notice}
      </div>
    );
  }
  const sender = members.find((m) => m.id === message.sender_user_id);
  return (
    <div className={styles.message} data-mine={message.mine ? "true" : undefined}>
      <div className={styles.bubble} data-status={message.status}>
        {showSender && sender && !message.mine ? (
          <span className={styles.sender}>{displayName(sender)}</span>
        ) : null}
        <MessageBodyView message={message} />
        <span className={styles.messageMeta}>
          <time dateTime={message.sent_at}>{formatDateTime(message.sent_at)}</time>
          {message.mine ? <> · {t(`chat.status.${message.status}`)}</> : null}
        </span>
      </div>
      {message.mine && message.status === "failed" ? (
        <Button size="sm" icon="refresh" onClick={() => onRetry(message)}>
          {t("chat.retry")}
        </Button>
      ) : null}
    </div>
  );
}

export function MessageList({
  label,
  messages,
  members,
  party,
  hasMore,
  loadingEarlier,
  earlierFailed,
  onLoadEarlier,
  onRetry,
}: {
  label: string;
  messages: readonly Message[];
  members: readonly UserSummary[];
  party: boolean;
  hasMore: boolean;
  loadingEarlier: boolean;
  earlierFailed: boolean;
  onLoadEarlier: () => Promise<number>;
  onRetry: (message: Message) => void;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const atBottom = useRef(true);
  const virtualizer = useVirtualizer({
    count: messages.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 64,
    overscan: 8,
    getItemKey: (index) => messages[index]?.id ?? index,
  });

  // Follow new messages while the reader is at the bottom (always for the first page).
  const last = messages[messages.length - 1]?.id;
  // biome-ignore lint/correctness/useExhaustiveDependencies: runs when the newest message changes.
  useLayoutEffect(() => {
    if (messages.length > 0 && atBottom.current)
      virtualizer.scrollToIndex(messages.length - 1, { align: "end" });
  }, [last]);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const onScroll = () => {
      atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, []);

  const loadEarlier = async () => {
    const added = await onLoadEarlier();
    // Keep the message that was on top where it was.
    if (added > 0) virtualizer.scrollToIndex(added, { align: "start" });
  };

  return (
    <>
      {hasMore || earlierFailed ? (
        <div className={styles.earlier}>
          {earlierFailed ? <span role="alert">{t("chat.earlierFailed")}</span> : null}
          <Button size="sm" loading={loadingEarlier} onClick={() => void loadEarlier()}>
            {loadingEarlier ? t("chat.loadingEarlier") : t("chat.loadEarlier")}
          </Button>
        </div>
      ) : null}
      {/* biome-ignore lint/a11y/noNoninteractiveTabindex: the scrolling history must be reachable to read it with keys. */}
      <div ref={scrollRef} className={styles.history} role="log" aria-label={label} tabIndex={0}>
        <ol className={styles.messages} style={{ height: virtualizer.getTotalSize() }}>
          {virtualizer.getVirtualItems().map((item) => {
            const message = messages[item.index];
            if (!message) return null;
            return (
              <li
                key={item.key}
                data-index={item.index}
                ref={virtualizer.measureElement}
                className={styles.messageItem}
                style={{ transform: `translateY(${item.start}px)` }}
              >
                <MessageRow
                  message={message}
                  members={members}
                  showSender={party}
                  onRetry={onRetry}
                />
              </li>
            );
          })}
        </ol>
      </div>
    </>
  );
}
