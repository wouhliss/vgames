// People blocked from this install on the active server (kept locally by the core).
import { useQuery } from "@tanstack/react-query";
import { Button } from "../../components/Button";
import { EmptyState, ErrorState, LoadingState } from "../../components/Feedback";
import { formatRelativeTime, t } from "../../i18n";
import { commands } from "../../ipc";
import { queryKeys, unwrap } from "../../ipc/query";
import styles from "./Friends.module.css";
import { useSocialAction } from "./useSocialAction";

export function BlockedTab() {
  const blocks = useQuery({
    queryKey: queryKeys.blocks,
    queryFn: async () => unwrap(await commands.blocksList()),
  });
  const { run, busy } = useSocialAction();

  if (blocks.isPending) return <LoadingState />;
  if (blocks.isError)
    return <ErrorState title={t("friends.loadFailed")} onRetry={() => void blocks.refetch()} />;
  if (blocks.data.length === 0)
    return (
      <EmptyState
        icon="block"
        title={t("friends.blockedList.emptyTitle")}
        description={t("friends.blockedList.emptyText")}
      />
    );

  return (
    <ul className={styles.list}>
      {blocks.data.map((block) => {
        const name = block.username ?? t("friends.blockedList.unknown");
        return (
          <li key={block.user_id} className={styles.row} data-nav-group="">
            <div className={styles.who}>
              <span className={styles.name}>{name}</span>
              <span className={styles.muted}>
                {t("friends.blockedList.since", { when: formatRelativeTime(block.blocked_at) })}
              </span>
            </div>
            <div className={styles.actions}>
              <Button
                loading={busy.has(block.user_id)}
                aria-label={`${t("friends.blockedList.unblock")}: ${name}`}
                onClick={async () => {
                  const done = await run(
                    block.user_id,
                    () => commands.userUnblock(block.user_id),
                    () => t("friends.blockedList.unblocked", { name }),
                  );
                  if (done) void blocks.refetch();
                }}
              >
                {t("friends.blockedList.unblock")}
              </Button>
            </div>
          </li>
        );
      })}
    </ul>
  );
}
