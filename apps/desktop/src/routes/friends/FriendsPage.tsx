// Friends (05-social): friends with presence, messages, requests and blocked people as tabs, each its
// own URL (/friends, /friends/messages[/<id>], /friends/requests, /friends/blocked). LB/RB switch tabs.
import { type ReactNode, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { useActiveServer } from "../../app/queries";
import { Button } from "../../components/Button";
import { ErrorState, LoadingState } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { Tabs } from "../../components/Tabs";
import { t } from "../../i18n";
import type { SocialError } from "../../ipc";
import { commandError } from "../../ipc/query";
import { Page } from "../Page";
import { AddFriendDialog } from "./AddFriendDialog";
import { BlockedTab } from "./BlockedTab";
import { MessagesTab } from "./chat/MessagesTab";
import styles from "./Friends.module.css";
import { FriendsTab } from "./FriendsTab";
import { socialErrorText } from "./model";
import { useConversations, useFriends, useSocialConnection } from "./queries";
import { RequestsTab } from "./RequestsTab";

type Tab = "friends" | "messages" | "requests" | "blocked";

const PATHS: Record<Tab, string> = {
  friends: "/friends",
  messages: "/friends/messages",
  requests: "/friends/requests",
  blocked: "/friends/blocked",
};

function parse(rest: string): { tab: Tab; conversationId: string | null } {
  const [first, second] = rest.split("/");
  if (first === "messages") return { tab: "messages", conversationId: second || null };
  if (first === "requests" || first === "blocked") return { tab: first, conversationId: null };
  return { tab: "friends", conversationId: null };
}

export function FriendsPage() {
  const { tab, conversationId } = parse(useParams()["*"] ?? "");
  const navigate = useNavigate();
  const friends = useFriends();
  const conversations = useConversations();
  const connection = useSocialConnection();
  const { server } = useActiveServer();
  const [adding, setAdding] = useState(false);
  const myId = server?.account?.user_id ?? null;

  const requests = (friends.data?.incoming.length ?? 0) + (friends.data?.outgoing.length ?? 0);
  const unread = (conversations.data ?? []).reduce((sum, c) => sum + c.unread, 0);
  const label = (value: Tab, count: number) =>
    count > 0
      ? t("friends.tabCount", { label: t(`friends.tab.${value}`), count })
      : t(`friends.tab.${value}`);

  const state = connection.data?.state;
  let content: ReactNode;
  if (tab === "blocked") content = <BlockedTab />;
  else if (tab === "messages")
    content = <MessagesTab conversationId={conversationId} myId={myId} />;
  else if (friends.isPending) content = <LoadingState />;
  else if (friends.isError)
    content = (
      <ErrorState
        title={t("friends.loadFailed")}
        description={socialErrorText(commandError<SocialError>(friends.error))}
        onRetry={() => void friends.refetch()}
      />
    );
  else if (tab === "requests") content = <RequestsTab list={friends.data} />;
  else content = <FriendsTab list={friends.data} onAdd={() => setAdding(true)} />;

  return (
    <Page title={t("friends.title")}>
      <div className={styles.toolbar}>
        <Button variant="primary" icon="plus" onClick={() => setAdding(true)}>
          {t("friends.add")}
        </Button>
      </div>
      {state === "reconnecting" ? (
        <Notice role="status">{t("friends.offlineNote")}</Notice>
      ) : state === "connecting" ? (
        <p role="status" className={styles.muted}>
          {t("friends.connecting")}
        </p>
      ) : null}
      <Tabs
        label={t("friends.tabs")}
        value={tab}
        onChange={(next) => navigate(PATHS[next])}
        items={[
          { value: "friends", label: t("friends.tab.friends") },
          { value: "messages", label: label("messages", unread) },
          { value: "requests", label: label("requests", requests) },
          { value: "blocked", label: t("friends.tab.blocked") },
        ]}
      >
        {content}
      </Tabs>
      <AddFriendDialog open={adding} onClose={() => setAdding(false)} />
    </Page>
  );
}
