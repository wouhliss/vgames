// One conversation's messages: the latest page, earlier pages on demand, and live updates from the
// core (new messages, delivery states, typing). Kept per open conversation, dropped when it closes.
import { useCallback, useEffect, useRef, useState } from "react";
import { commands, events, type Message, type SocialError } from "../../../ipc";
import { useTauriEvent } from "../../../ipc/events";

export const PAGE = 50;
const TYPING_MS = 5000;

/**
 * Adds or replaces a message. With `keepDelivery`, a delivery state the core already reported
 * (`message-status-changed` can arrive before the send command's own reply) is not reset to pending.
 */
export function upsert(
  list: readonly Message[],
  message: Message,
  keepDelivery = false,
): Message[] {
  const i = list.findIndex((m) => m.id === message.id);
  if (i < 0) return [...list, message];
  const next = [...list];
  const current = list[i];
  next[i] =
    keepDelivery && current && current.status !== "pending"
      ? { ...message, status: current.status }
      : message;
  return next;
}

export function useMessages(conversationId: string) {
  const [messages, setMessages] = useState<Message[]>([]);
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");
  const [error, setError] = useState<SocialError | null>(null);
  const [hasMore, setHasMore] = useState(false);
  const [loadingEarlier, setLoadingEarlier] = useState(false);
  const [earlierFailed, setEarlierFailed] = useState(false);
  const [typing, setTyping] = useState<ReadonlySet<string>>(() => new Set());
  const timers = useRef(new Map<string, ReturnType<typeof setTimeout>>());

  const load = useCallback(async () => {
    setStatus("loading");
    try {
      const result = await commands.messagesList(conversationId, null, PAGE);
      if (result.status === "error") {
        setError(result.error);
        setStatus("error");
        return;
      }
      setMessages(result.data);
      setHasMore(result.data.length === PAGE);
      setStatus("ready");
    } catch {
      setError(null);
      setStatus("error");
    }
  }, [conversationId]);

  useEffect(() => {
    setMessages([]);
    setTyping(new Set());
    void load();
  }, [load]);

  useEffect(() => {
    const pending = timers.current;
    return () => {
      for (const timer of pending.values()) clearTimeout(timer);
      pending.clear();
    };
  }, []);

  /** Loads the page before the oldest message; resolves to how many were added. */
  const loadEarlier = async (): Promise<number> => {
    const oldest = messages[0];
    if (!oldest || loadingEarlier) return 0;
    setLoadingEarlier(true);
    setEarlierFailed(false);
    try {
      const result = await commands.messagesList(conversationId, oldest.id, PAGE);
      if (result.status === "error") {
        setEarlierFailed(true);
        return 0;
      }
      const known = new Set(messages.map((m) => m.id));
      const added = result.data.filter((m) => !known.has(m.id));
      setMessages((prev) => [...added, ...prev]);
      setHasMore(result.data.length === PAGE);
      return added.length;
    } catch {
      setEarlierFailed(true);
      return 0;
    } finally {
      setLoadingEarlier(false);
    }
  };

  useTauriEvent(events.messageReceived, (message) => {
    if (message.conversation_id !== conversationId) return;
    setMessages((prev) => upsert(prev, message));
    // A message from someone ends their "is typing".
    setTyping((prev) => {
      if (!prev.has(message.sender_user_id)) return prev;
      const next = new Set(prev);
      next.delete(message.sender_user_id);
      return next;
    });
  });
  useTauriEvent(events.messageStatusChanged, (change) => {
    if (change.conversation_id !== conversationId) return;
    setMessages((prev) =>
      prev.map((m) => (m.id === change.message_id ? { ...m, status: change.status } : m)),
    );
  });
  useTauriEvent(events.typing, ({ conversation_id, user_id }) => {
    if (conversation_id !== conversationId) return;
    setTyping((prev) => new Set(prev).add(user_id));
    clearTimeout(timers.current.get(user_id));
    timers.current.set(
      user_id,
      setTimeout(() => {
        timers.current.delete(user_id);
        setTyping((prev) => {
          const next = new Set(prev);
          next.delete(user_id);
          return next;
        });
      }, TYPING_MS),
    );
  });

  /** Adds a message this window sent; `retried` resets its delivery state to pending. */
  const put = (message: Message, retried = false) =>
    setMessages((prev) => upsert(prev, message, !retried));

  return {
    messages,
    status,
    error,
    hasMore,
    loadingEarlier,
    earlierFailed,
    typing,
    reload: load,
    loadEarlier,
    put,
  };
}
