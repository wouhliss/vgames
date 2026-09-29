// The friends list, grouped Playing / Online / Offline, with Message, Invite to play and a menu per
// friend (safety number, remove, block). Invites you sent follow, with the invitee's progress.
import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useNavigate } from "react-router";
import { Button, IconButton } from "../../components/Button";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/Feedback";
import { Menu } from "../../components/Menu";
import { t } from "../../i18n";
import { commands, type Friend, type FriendList } from "../../ipc";
import { queryKeys } from "../../ipc/query";
import { Avatar } from "./Avatar";
import styles from "./Friends.module.css";
import { InviteDialog } from "./InviteDialog";
import { OutgoingInvites } from "./invites";
import { displayName, type PresenceGroup, presenceGroup, presenceText, sortFriends } from "./model";
import { SafetyNumberDialog } from "./SafetyNumberDialog";
import { useSocialAction } from "./useSocialAction";

type Pending =
  | { kind: "remove"; friend: Friend }
  | { kind: "block"; friend: Friend }
  | { kind: "invite"; friend: Friend }
  | { kind: "safety"; friend: Friend }
  | null;

const GROUPS: PresenceGroup[] = ["playing", "online", "offline"];

export function FriendsTab({ list, onAdd }: { list: FriendList; onAdd: () => void }) {
  const navigate = useNavigate();
  const client = useQueryClient();
  const { run, busy } = useSocialAction();
  const [pending, setPending] = useState<Pending>(null);
  const sorted = sortFriends(list.friends);

  const openChat = async (friend: Friend) => {
    const opened = await run(`chat:${friend.user.id}`, () =>
      commands.conversationOpenDirect(friend.user.id),
    );
    if (opened) {
      await client.invalidateQueries({ queryKey: queryKeys.conversations });
      navigate(`/friends/messages/${opened.data.id}`);
    }
  };

  if (sorted.length === 0) {
    return (
      <>
        <EmptyState
          icon="friends"
          title={t("friends.empty.title")}
          description={t("friends.empty.text")}
          action={
            <Button variant="primary" icon="plus" onClick={onAdd}>
              {t("friends.add")}
            </Button>
          }
        />
        <OutgoingInvites />
      </>
    );
  }

  return (
    <div className={styles.stack}>
      {GROUPS.map((group) => {
        const members = sorted.filter((f) => presenceGroup(f.presence) === group);
        if (members.length === 0) return null;
        const headingId = `friends-group-${group}`;
        return (
          <section key={group} aria-labelledby={headingId}>
            <h2 id={headingId} className={styles.groupTitle}>
              {t(`friends.group.${group}`)} · {members.length}
            </h2>
            <ul className={styles.list}>
              {members.map((friend) => {
                const name = displayName(friend.user);
                return (
                  <li key={friend.user.id} className={styles.row} data-nav-group="">
                    <Avatar user={friend.user} status={friend.presence?.status ?? null} />
                    <div className={styles.who}>
                      <span className={styles.name}>{name}</span>
                      <span className={styles.muted}>{presenceText(friend.presence)}</span>
                    </div>
                    <div className={styles.actions}>
                      <Button
                        icon="chat"
                        loading={busy.has(`chat:${friend.user.id}`)}
                        aria-label={`${t("friends.message")}: ${name}`}
                        onClick={() => void openChat(friend)}
                      >
                        {t("friends.message")}
                      </Button>
                      <Button
                        icon="play"
                        aria-label={`${t("friends.inviteToPlay")}: ${name}`}
                        onClick={() => setPending({ kind: "invite", friend })}
                      >
                        {t("friends.inviteToPlay")}
                      </Button>
                      <Menu
                        label={t("friends.more", { name })}
                        entries={[
                          {
                            id: "safety",
                            label: t("friends.safetyNumber"),
                            icon: "shield",
                            onSelect: () => setPending({ kind: "safety", friend }),
                          },
                          { id: "sep", separator: true },
                          {
                            id: "remove",
                            label: t("friends.remove"),
                            icon: "trash",
                            onSelect: () => setPending({ kind: "remove", friend }),
                          },
                          {
                            id: "block",
                            label: t("friends.block"),
                            icon: "block",
                            danger: true,
                            onSelect: () => setPending({ kind: "block", friend }),
                          },
                        ]}
                        trigger={<IconButton icon="more" label={t("friends.more", { name })} />}
                      />
                    </div>
                  </li>
                );
              })}
            </ul>
          </section>
        );
      })}
      <OutgoingInvites />

      <ConfirmDialog
        open={pending?.kind === "remove"}
        title={pending ? t("friends.removeTitle", { name: displayName(pending.friend.user) }) : ""}
        description={t("friends.removeText")}
        confirmLabel={t("friends.remove")}
        tone="danger"
        onCancel={() => setPending(null)}
        onConfirm={async () => {
          if (!pending) return;
          const name = displayName(pending.friend.user);
          await run(
            "remove",
            () => commands.friendRemove(pending.friend.user.id),
            () => t("friends.removed", { name }),
          );
          setPending(null);
        }}
      />
      <BlockDialog
        friend={pending?.kind === "block" ? pending.friend : null}
        onDone={() => setPending(null)}
      />
      {pending?.kind === "invite" ? (
        <InviteDialog friend={pending.friend.user} onClose={() => setPending(null)} />
      ) : null}
      {pending?.kind === "safety" ? (
        <SafetyNumberDialog user={pending.friend.user} onClose={() => setPending(null)} />
      ) : null}
    </div>
  );
}

/** Confirms blocking someone (friend, requester or anyone else shown in the social screens). */
export function BlockDialog({ friend, onDone }: { friend: Friend | null; onDone: () => void }) {
  const { run } = useSocialAction();
  const name = friend ? displayName(friend.user) : "";
  return (
    <ConfirmDialog
      open={friend !== null}
      title={t("friends.blockTitle", { name })}
      description={t("friends.blockText")}
      confirmLabel={t("friends.block")}
      tone="danger"
      onCancel={onDone}
      onConfirm={async () => {
        if (!friend) return;
        await run(
          "block",
          () => commands.userBlock(friend.user.id),
          () => t("friends.blocked", { name }),
        );
        onDone();
      }}
    />
  );
}
