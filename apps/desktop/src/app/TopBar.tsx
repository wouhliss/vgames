import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { NavLink, useNavigate } from "react-router";
import { Button } from "../components/Button";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { Icon } from "../components/Icon";
import { Menu, type MenuEntry } from "../components/Menu";
import { useToast } from "../components/Toast";
import { Tooltip } from "../components/Tooltip";
import { formatPercent, t } from "../i18n";
import { commands, events, type InstallProgress, type ServerProfile } from "../ipc";
import { useTauriEvent } from "../ipc/events";
import { queryKeys } from "../ipc/query";
import styles from "./Shell.module.css";

export function ServerSwitcher({
  servers,
  active,
}: {
  servers: readonly ServerProfile[];
  active: ServerProfile;
}) {
  const navigate = useNavigate();
  const client = useQueryClient();
  const { toast } = useToast();
  const entries: MenuEntry[] = [
    ...servers.map((server) => ({
      id: server.id,
      label: server.name,
      icon: server.active ? ("check" as const) : ("server" as const),
      hint: server.active ? t("shell.activeServer") : undefined,
      onSelect: async () => {
        if (server.active) return;
        const result = await commands.serverSwitch(server.id);
        if (result.status === "ok") {
          await client.invalidateQueries();
          toast({ tone: "success", title: t("shell.switched", { name: server.name }) });
        } else {
          toast({ tone: "danger", title: t("shell.switchFailed") });
        }
      },
    })),
    { id: "sep", separator: true },
    {
      id: "add",
      label: t("shell.addServer"),
      icon: "plus",
      onSelect: () => navigate("/onboarding?add=1"),
    },
  ];
  return (
    <Menu
      label={t("shell.serverMenu")}
      entries={entries}
      trigger={
        <Button
          variant="ghost"
          icon="server"
          aria-label={t("shell.serverSwitcher", { name: active.name })}
        >
          <span className={styles.menuButtonLabel}>{active.name}</span>
          <Icon name="chevronDown" size={16} />
        </Button>
      }
    />
  );
}

export function AccountMenu({ server }: { server: ServerProfile }) {
  const [confirm, setConfirm] = useState(false);
  const client = useQueryClient();
  const { toast } = useToast();
  const account = server.account;
  if (!account) return null;
  const name = account.display_name ?? account.username;
  return (
    <>
      <Menu
        label={t("shell.accountMenu", { name })}
        entries={[
          {
            id: "who",
            label: t("shell.signedInAs", { name }),
            icon: "user",
            disabled: true,
            onSelect: () => {},
          },
          { id: "sep", separator: true },
          { id: "signout", label: t("shell.signOut"), onSelect: () => setConfirm(true) },
        ]}
        trigger={
          <Button variant="ghost" icon="user" aria-label={t("shell.accountMenu", { name })}>
            <span className={styles.menuButtonLabel}>{name}</span>
          </Button>
        }
      />
      <ConfirmDialog
        open={confirm}
        title={t("shell.signOutTitle", { server: server.name })}
        description={t("shell.signOutText")}
        confirmLabel={t("shell.signOut")}
        onCancel={() => setConfirm(false)}
        onConfirm={async () => {
          const result = await commands.authSignOut(server.id);
          setConfirm(false);
          if (result.status === "ok")
            await client.invalidateQueries({ queryKey: queryKeys.servers });
          else toast({ tone: "danger", title: t("shell.signOutFailed") });
        }}
      />
    </>
  );
}

/** Top-bar entry to Downloads, showing the active install's progress (event-driven, no polling). */
export function DownloadIndicator() {
  const [progress, setProgress] = useState<InstallProgress | null>(null);
  useTauriEvent(events.installProgress, setProgress);
  useTauriEvent(events.installFinished, () => setProgress(null));
  const fraction =
    progress && progress.bytes_total > 0 ? progress.bytes_done / progress.bytes_total : null;
  const label =
    fraction !== null
      ? t("shell.downloadsActive", { percent: formatPercent(fraction) })
      : t("shell.downloadsIdle");
  return (
    <Tooltip content={label}>
      <NavLink to="/downloads" className={styles.navLink ?? ""} aria-label={label}>
        <Icon name="download" />
        {fraction !== null ? (
          <span className={styles.downloadPercent}>{formatPercent(fraction)}</span>
        ) : null}
      </NavLink>
    </Tooltip>
  );
}

/** Slot for the launcher update banner (A3-T10). */
export function UpdateBannerSlot(): null {
  return null;
}
