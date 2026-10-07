// Account on the active server: who you're signed in as, where else you're signed in (sign out a
// device), sign out here, and a warning when sign-in tokens live in a file instead of the keychain.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useState } from "react";
import { useActiveServer } from "../../app/queries";
import { Button } from "../../components/Button";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { Badge, ErrorState, LoadingState } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { useToast } from "../../components/Toast";
import { formatRelativeTime, t } from "../../i18n";
import { type AccountSession, commands } from "../../ipc";

/** What to call a session: the device's name, else the client it signed in with. */
const deviceName = (session: AccountSession) =>
  session.device_name ?? session.user_agent ?? t("settings.account.unknownDevice");

import { queryKeys, unwrap } from "../../ipc/query";
import styles from "./Settings.module.css";
import { Section } from "./SettingsPage";

export function AccountSection() {
  const { server } = useActiveServer();
  const client = useQueryClient();
  const { toast } = useToast();
  const [revoking, setRevoking] = useState<AccountSession | null>(null);
  const [signingOut, setSigningOut] = useState(false);
  const serverId = server?.id ?? "";
  const signedIn = Boolean(server?.account);
  const sessions = useQuery({
    queryKey: ["account_sessions", serverId],
    queryFn: async () => unwrap(await commands.accountSessions(serverId)),
    enabled: signedIn,
  });
  const storage = useQuery({
    queryKey: ["auth_token_storage"],
    queryFn: () => commands.authTokenStorage(),
  });

  if (!server) return null;
  const account = server.account;

  const revoke = async (session: AccountSession) => {
    const result = await commands.accountSessionRevoke(server.id, session.id).catch(() => null);
    setRevoking(null);
    if (result?.status === "ok") {
      await client.invalidateQueries({ queryKey: ["account_sessions", server.id] });
      toast({
        tone: "success",
        title: t("settings.account.revoked", { device: deviceName(session) }),
      });
    } else toast({ tone: "danger", title: t("settings.account.revokeFailed") });
  };

  const signOut = async () => {
    const result = await commands.authSignOut(server.id).catch(() => null);
    setSigningOut(false);
    if (result?.status === "ok") await client.invalidateQueries({ queryKey: queryKeys.servers });
    else toast({ tone: "danger", title: t("shell.signOutFailed") });
  };

  let list: ReactNode = null;
  if (signedIn) {
    if (sessions.data) {
      list = (
        <ul className={styles.rows} data-nav-group="">
          {sessions.data.map((session) => (
            <li key={session.id} className={styles.row}>
              <span className={styles.rowMain}>
                <span className={styles.rowTitle}>{deviceName(session)}</span>
                <span className={styles.muted}>
                  {t(`settings.account.client.${session.client}`)} ·{" "}
                  {t("settings.account.lastUsed", {
                    when: formatRelativeTime(session.last_used_at),
                  })}
                </span>
              </span>
              {session.current ? (
                <Badge tone="accent">{t("settings.account.thisDevice")}</Badge>
              ) : (
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={t("settings.account.revokeTitle", { device: deviceName(session) })}
                  onClick={() => setRevoking(session)}
                >
                  {t("settings.account.revoke")}
                </Button>
              )}
            </li>
          ))}
        </ul>
      );
    } else if (sessions.isError) {
      list = (
        <ErrorState
          title={t("settings.account.loadFailed")}
          onRetry={() => void sessions.refetch()}
        />
      );
    } else list = <LoadingState />;
  }

  return (
    <Section id="account" title={t("settings.section.account")}>
      {storage.data?.fallback_file ? (
        <Notice tone="warning" role="status">
          <strong>{t("settings.account.keychainTitle")}</strong>{" "}
          {t("settings.account.keychainText")}
        </Notice>
      ) : null}
      {account ? (
        <div className={styles.actions}>
          <p>
            {t("settings.account.signedInAs", {
              server: server.name,
              name: account.display_name ?? account.username,
            })}
          </p>
          <Button onClick={() => setSigningOut(true)}>{t("shell.signOut")}</Button>
        </div>
      ) : null}
      {signedIn ? (
        <div className={styles.subsection}>
          <h3>{t("settings.account.sessions")}</h3>
          <p className={styles.muted}>{t("settings.account.sessionsText")}</p>
          {list}
        </div>
      ) : null}
      <ConfirmDialog
        open={revoking !== null}
        tone="danger"
        title={t("settings.account.revokeConfirmTitle", {
          device: revoking ? deviceName(revoking) : "",
        })}
        description={t("settings.account.revokeConfirmText")}
        confirmLabel={t("settings.account.revoke")}
        onCancel={() => setRevoking(null)}
        onConfirm={() => (revoking ? revoke(revoking) : undefined)}
      />
      <ConfirmDialog
        open={signingOut}
        title={t("shell.signOutTitle", { server: server.name })}
        description={t("shell.signOutText")}
        confirmLabel={t("shell.signOut")}
        onCancel={() => setSigningOut(false)}
        onConfirm={signOut}
      />
    </Section>
  );
}
