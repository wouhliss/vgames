// Servers: every server added, with its address and fingerprint; switch, remove, add another.
import { useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useState } from "react";
import { useNavigate } from "react-router";
import { useServers } from "../../app/queries";
import { Button } from "../../components/Button";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { Badge, ErrorState, LoadingState } from "../../components/Feedback";
import { Fingerprint } from "../../components/Notice";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import { commands, type ServerProfile } from "../../ipc";
import { queryKeys } from "../../ipc/query";
import styles from "./Settings.module.css";
import { Section } from "./SettingsPage";

export function ServersSection() {
  const servers = useServers();
  const navigate = useNavigate();
  const client = useQueryClient();
  const { toast } = useToast();
  const [removing, setRemoving] = useState<ServerProfile | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const switchTo = async (server: ServerProfile) => {
    setBusy(server.id);
    const result = await commands.serverSwitch(server.id).catch(() => null);
    setBusy(null);
    if (result?.status === "ok") {
      // Everything shown belongs to a server: refresh it all.
      await client.invalidateQueries();
      toast({ tone: "success", title: t("shell.switched", { name: server.name }) });
    } else toast({ tone: "danger", title: t("shell.switchFailed") });
  };

  const remove = async (server: ServerProfile) => {
    const result = await commands.serverRemove(server.id).catch(() => null);
    setRemoving(null);
    if (result?.status === "ok") {
      await client.invalidateQueries({ queryKey: queryKeys.servers });
      toast({ tone: "success", title: t("settings.servers.removed", { name: server.name }) });
    } else toast({ tone: "danger", title: t("settings.servers.removeFailed") });
  };

  let body: ReactNode;
  if (servers.data) {
    body = (
      <ul className={styles.cards} data-nav-group="">
        {servers.data.map((server) => {
          const account = server.account;
          return (
            <li key={server.id} className={styles.card} aria-labelledby={`server-${server.id}`}>
              <div className={styles.cardHeader}>
                <h3 id={`server-${server.id}`}>{server.name}</h3>
                {server.active ? (
                  <Badge tone="success">{t("settings.servers.active")}</Badge>
                ) : null}
              </div>
              <dl className={styles.facts}>
                <div>
                  <dt>{t("settings.servers.address")}</dt>
                  <dd data-selectable="">{server.url}</dd>
                </div>
              </dl>
              <Fingerprint value={server.fingerprint} label={t("settings.servers.fingerprint")} />
              <p className={styles.muted}>
                {account
                  ? t("settings.servers.signedInAs", {
                      name: account.display_name ?? account.username,
                    })
                  : t("settings.servers.signedOut")}
              </p>
              <div className={styles.actions}>
                {server.active ? null : (
                  <Button
                    size="sm"
                    variant="primary"
                    loading={busy === server.id}
                    aria-label={t("settings.servers.switchTitle", { name: server.name })}
                    onClick={() => void switchTo(server)}
                  >
                    {t("settings.servers.switch")}
                  </Button>
                )}
                <Button
                  size="sm"
                  variant="ghost"
                  icon="trash"
                  aria-label={t("settings.servers.removeTitle", { name: server.name })}
                  onClick={() => setRemoving(server)}
                >
                  {t("settings.servers.remove")}
                </Button>
              </div>
            </li>
          );
        })}
      </ul>
    );
  } else if (servers.isError) {
    body = <ErrorState title={t("error.generic")} onRetry={() => void servers.refetch()} />;
  } else body = <LoadingState />;

  return (
    <Section id="servers" title={t("settings.section.servers")} text={t("settings.servers.text")}>
      {body}
      <div>
        <Button icon="plus" onClick={() => navigate("/onboarding?add=1")}>
          {t("settings.servers.add")}
        </Button>
      </div>
      <ConfirmDialog
        open={removing !== null}
        tone="danger"
        title={t("settings.servers.removeConfirmTitle", { name: removing?.name ?? "" })}
        description={t("settings.servers.removeConfirmText")}
        confirmLabel={t("settings.servers.removeConfirm")}
        onCancel={() => setRemoving(null)}
        onConfirm={() => (removing ? remove(removing) : undefined)}
      />
    </Section>
  );
}
