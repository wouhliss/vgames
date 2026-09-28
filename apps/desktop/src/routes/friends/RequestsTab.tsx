// Friend requests: incoming (accept, decline, block) and outgoing (cancel).
import { useState } from "react";
import { Button } from "../../components/Button";
import { EmptyState } from "../../components/Feedback";
import { t } from "../../i18n";
import { commands, type Friend, type FriendList } from "../../ipc";
import { Avatar } from "./Avatar";
import styles from "./Friends.module.css";
import { BlockDialog } from "./FriendsTab";
import { displayName } from "./model";
import { useSocialAction } from "./useSocialAction";

export function RequestsTab({ list }: { list: FriendList }) {
  const { run, busy } = useSocialAction();
  const [blocking, setBlocking] = useState<Friend | null>(null);

  if (list.incoming.length === 0 && list.outgoing.length === 0) {
    return (
      <EmptyState
        icon="friends"
        title={t("friends.requests.emptyTitle")}
        description={t("friends.requests.emptyText")}
      />
    );
  }

  return (
    <div className={styles.stack}>
      {list.incoming.length > 0 ? (
        <section aria-labelledby="requests-incoming">
          <h2 id="requests-incoming" className={styles.groupTitle}>
            {t("friends.requests.incoming")} · {list.incoming.length}
          </h2>
          <ul className={styles.list}>
            {list.incoming.map((request) => {
              const name = displayName(request.user);
              const id = request.user.id;
              return (
                <li key={id} className={styles.row} data-nav-group="">
                  <Avatar user={request.user} />
                  <div className={styles.who}>
                    <span className={styles.name}>{name}</span>
                    <span className={styles.muted}>{t("friends.requests.wantsToBeFriends")}</span>
                  </div>
                  <div className={styles.actions}>
                    <Button
                      variant="primary"
                      icon="check"
                      loading={busy.has(`accept:${id}`)}
                      aria-label={`${t("friends.requests.accept")}: ${name}`}
                      onClick={() =>
                        void run(
                          `accept:${id}`,
                          () => commands.friendAccept(id),
                          () => t("friends.requests.accepted", { name }),
                        )
                      }
                    >
                      {t("friends.requests.accept")}
                    </Button>
                    <Button
                      loading={busy.has(`decline:${id}`)}
                      aria-label={`${t("friends.requests.decline")}: ${name}`}
                      onClick={() =>
                        void run(
                          `decline:${id}`,
                          () => commands.friendDecline(id),
                          () => t("friends.requests.declined", { name }),
                        )
                      }
                    >
                      {t("friends.requests.decline")}
                    </Button>
                    <Button
                      variant="ghost"
                      icon="block"
                      aria-label={`${t("friends.block")}: ${name}`}
                      onClick={() => setBlocking(request)}
                    >
                      {t("friends.block")}
                    </Button>
                  </div>
                </li>
              );
            })}
          </ul>
        </section>
      ) : null}
      {list.outgoing.length > 0 ? (
        <section aria-labelledby="requests-outgoing">
          <h2 id="requests-outgoing" className={styles.groupTitle}>
            {t("friends.requests.outgoing")} · {list.outgoing.length}
          </h2>
          <ul className={styles.list}>
            {list.outgoing.map((request) => {
              const name = displayName(request.user);
              const id = request.user.id;
              return (
                <li key={id} className={styles.row} data-nav-group="">
                  <Avatar user={request.user} />
                  <div className={styles.who}>
                    <span className={styles.name}>{name}</span>
                    <span className={styles.muted}>{t("friends.requests.sentAgo")}</span>
                  </div>
                  <div className={styles.actions}>
                    <Button
                      loading={busy.has(`cancel:${id}`)}
                      aria-label={`${t("friends.requests.cancel")}: ${name}`}
                      onClick={() =>
                        void run(
                          `cancel:${id}`,
                          () => commands.friendRemove(id),
                          () => t("friends.requests.cancelled", { name }),
                        )
                      }
                    >
                      {t("friends.requests.cancel")}
                    </Button>
                  </div>
                </li>
              );
            })}
          </ul>
        </section>
      ) : null}
      <BlockDialog friend={blocking} onDone={() => setBlocking(null)} />
    </div>
  );
}
