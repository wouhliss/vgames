// A round avatar with the person's initial and, optionally, a presence dot. Avatars from the server
// are not loaded directly (the WebView never fetches): initials only until the core serves them.
import type { PresenceStatus, UserSummary } from "../../ipc";
import styles from "./Friends.module.css";
import { displayName } from "./model";

export function Avatar({
  user,
  status,
  size = "md",
}: {
  user: UserSummary;
  status?: PresenceStatus | null | undefined;
  size?: "sm" | "md";
}) {
  const initial = [...displayName(user)][0]?.toUpperCase() ?? "?";
  return (
    <span className={styles.avatar} data-size={size} aria-hidden="true">
      {initial}
      {status !== undefined ? (
        <span className={styles.dot} data-status={status ?? "offline"} />
      ) : null}
    </span>
  );
}
