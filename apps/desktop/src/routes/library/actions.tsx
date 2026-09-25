// What a library tile can do: the primary action (Play / Stop / Resume / status), the actions menu,
// and the dialogs those open. Shared through context so virtualized tiles stay cheap.
import { useQueryClient } from "@tanstack/react-query";
import { createContext, type ReactNode, useCallback, useContext, useMemo, useState } from "react";
import { useNavigate } from "react-router";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import type { IconName } from "../../components/Icon";
import type { MenuEntry } from "../../components/Menu";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import {
  type Collection,
  commands,
  type InstallActionError,
  type InstalledPackage,
  type Library,
  type Result,
} from "../../ipc";
import { queryKeys } from "../../ipc/query";
import {
  AddToCollectionDialog,
  ManageCollectionsDialog,
  MoveDialog,
  UninstallDialog,
} from "./dialogs";
import { actionErrorMessage, launchErrorMessage } from "./messages";
import { isOffline } from "./model";

type DialogState =
  | { kind: "none" }
  | { kind: "collections" }
  | { kind: "add_to_collection"; pkg: InstalledPackage }
  | { kind: "move"; pkg: InstalledPackage }
  | { kind: "uninstall"; pkg: InstalledPackage }
  | { kind: "stop"; pkg: InstalledPackage }
  | { kind: "yanked_update"; pkg: InstalledPackage };

export type PrimaryAction =
  | { kind: "play" }
  | { kind: "offline"; reason: string }
  | { kind: "stop" }
  | { kind: "resume" }
  | { kind: "download" }
  | { kind: "busy"; label: string };

export interface LibraryActions {
  primary: (pkg: InstalledPackage) => PrimaryAction;
  runPrimary: (pkg: InstalledPackage) => void;
  entries: (pkg: InstalledPackage) => MenuEntry[];
  openDetails: (pkg: InstalledPackage) => void;
  /** Package ids whose launch is in flight. */
  launching: ReadonlySet<string>;
  openCollections: () => void;
}

const LibraryActionsContext = createContext<LibraryActions | null>(null);

export function useLibraryActionsContext(): LibraryActions {
  const ctx = useContext(LibraryActionsContext);
  if (!ctx)
    throw new Error("useLibraryActionsContext must be used inside <LibraryActionsProvider>");
  return ctx;
}

const BUSY_LABEL: Record<
  Exclude<InstalledPackage["state"], "installed" | "incomplete">,
  () => string
> = {
  installing: () => t("library.badges.installing"),
  updating: () => t("library.badges.updating"),
  repairing: () => t("library.badges.repairing"),
  moving: () => t("library.badges.moving"),
  uninstalling: () => t("library.badges.uninstalling"),
};

export function LibraryActionsProvider({
  libraries,
  collections,
  installs,
  children,
}: {
  libraries: readonly Library[];
  collections: readonly Collection[];
  installs: readonly InstalledPackage[];
  children: ReactNode;
}) {
  const [dialog, setDialog] = useState<DialogState>({ kind: "none" });
  const [launching, setLaunching] = useState<ReadonlySet<string>>(() => new Set());
  const { toast } = useToast();
  const navigate = useNavigate();
  const client = useQueryClient();
  const close = useCallback(() => setDialog({ kind: "none" }), []);

  const refresh = useCallback(
    () => client.invalidateQueries({ queryKey: queryKeys.installs }),
    [client],
  );

  /** Runs an install action and reports the outcome as a toast. */
  const run = useCallback(
    async (
      pkg: InstalledPackage,
      action: () => Promise<Result<null, InstallActionError>>,
      success: { title: string; downloads?: boolean },
    ) => {
      try {
        const result = await action();
        if (result.status === "error") {
          toast({ tone: "danger", title: actionErrorMessage(result.error, pkg.title) });
          return;
        }
        await refresh();
        toast({
          tone: "info",
          title: success.title,
          ...(success.downloads
            ? {
                action: {
                  label: t("library.toasts.viewDownloads"),
                  onClick: () => navigate("/downloads"),
                },
              }
            : {}),
        });
      } catch {
        toast({ tone: "danger", title: t("error.generic") });
      }
    },
    [navigate, refresh, toast],
  );

  const verify = useCallback(
    (pkg: InstalledPackage) =>
      run(pkg, () => commands.installVerify(pkg.package), {
        title: t("library.toasts.verifyStarted", { title: pkg.title }),
      }),
    [run],
  );

  const launch = useCallback(
    async (pkg: InstalledPackage, targetId: string | null) => {
      const id = pkg.package.package_id;
      setLaunching((prev) => new Set(prev).add(id));
      try {
        const result = await commands.gameLaunch(pkg.package, targetId);
        if (result.status === "ok") {
          toast({
            tone: "info",
            title: t("library.toasts.starting", { title: pkg.title }),
            duration: 3000,
          });
          return;
        }
        const error = result.error;
        toast({
          tone: error.kind === "save_conflict" ? "warning" : "danger",
          title: launchErrorMessage(error, pkg.title),
          ...(error.kind === "integrity" || error.kind === "key_revoked"
            ? { action: { label: t("library.toasts.verify"), onClick: () => void verify(pkg) } }
            : {}),
        });
      } catch {
        toast({ tone: "danger", title: t("error.generic") });
      } finally {
        setLaunching((prev) => {
          const next = new Set(prev);
          next.delete(id);
          return next;
        });
      }
    },
    [toast, verify],
  );

  const primary = useCallback(
    (pkg: InstalledPackage): PrimaryAction => {
      if (pkg.running) return { kind: "stop" };
      if (pkg.state === "incomplete") return { kind: "resume" };
      if (pkg.state === "installing" || pkg.state === "updating" || pkg.state === "repairing")
        return { kind: "download" };
      if (pkg.state !== "installed") return { kind: "busy", label: BUSY_LABEL[pkg.state]() };
      if (isOffline(pkg, libraries)) {
        const path = libraries.find((l) => l.id === pkg.library_id)?.path ?? "";
        return { kind: "offline", reason: t("library.offlineReason", { path }) };
      }
      return { kind: "play" };
    },
    [libraries],
  );

  const runPrimary = useCallback(
    (pkg: InstalledPackage) => {
      const action = primary(pkg);
      switch (action.kind) {
        case "play":
          if (!launching.has(pkg.package.package_id)) void launch(pkg, null);
          return;
        case "stop":
          setDialog({ kind: "stop", pkg });
          return;
        case "resume":
          void run(pkg, () => commands.installResume(pkg.package), {
            title: t("library.toasts.resumeQueued", { title: pkg.title }),
            downloads: true,
          });
          return;
        case "download":
          navigate("/downloads");
          return;
        case "offline":
        case "busy":
          return;
      }
    },
    [launch, launching, navigate, primary, run],
  );

  const toggleFavorite = useCallback(
    async (pkg: InstalledPackage) => {
      try {
        const result = await commands.favoriteSet(pkg.package, !pkg.favorite);
        if (result.status === "error") throw new Error(result.error.kind);
        await refresh();
      } catch {
        toast({ tone: "danger", title: t("library.toasts.favoriteFailed") });
      }
    },
    [refresh, toast],
  );

  const entries = useCallback(
    (pkg: InstalledPackage): MenuEntry[] => {
      const installed = pkg.state === "installed";
      const idle = installed && !pkg.running && !isOffline(pkg, libraries);
      const out: MenuEntry[] = [];
      if (pkg.targets.length > 1) {
        for (const target of pkg.targets) {
          out.push({
            id: `target-${target.id}`,
            label: t("library.actions.playTarget", { target: target.label }),
            icon: "play" as IconName,
            disabled: !idle,
            onSelect: () => void launch(pkg, target.id),
          });
        }
        out.push({ id: "sep-targets", separator: true });
      }
      out.push(
        {
          id: "details",
          label: t("library.actions.details"),
          icon: "info",
          onSelect: () => navigate(`/package/${pkg.package.package_id}`),
        },
        {
          id: "favorite",
          label: pkg.favorite
            ? t("library.actions.favoriteRemove")
            : t("library.actions.favoriteAdd"),
          icon: "star",
          onSelect: () => void toggleFavorite(pkg),
        },
        {
          id: "collection",
          label: t("library.actions.addToCollection"),
          icon: "collection",
          onSelect: () => setDialog({ kind: "add_to_collection", pkg }),
        },
        { id: "sep-manage", separator: true },
      );
      if (pkg.update) {
        const update = pkg.update;
        out.push({
          id: "update",
          label: t("library.actions.update", { version: update.version_label }),
          icon: "download",
          disabled: !idle,
          onSelect: () => {
            if (update.installed_yanked) setDialog({ kind: "yanked_update", pkg });
            else
              void run(pkg, () => commands.installUpdate(pkg.package), {
                title: t("library.toasts.updateQueued", { title: pkg.title }),
                downloads: true,
              });
          },
        });
      }
      if (installed) {
        out.push(
          {
            id: "verify",
            label: t("library.actions.verify"),
            icon: "shield",
            disabled: !idle,
            onSelect: () => void verify(pkg),
          },
          {
            id: "move",
            label: t("library.actions.move"),
            icon: "folder",
            disabled: !idle,
            onSelect: () => setDialog({ kind: "move", pkg }),
          },
          {
            id: "shortcut",
            label: t("library.actions.shortcut"),
            icon: "external",
            onSelect: async () => {
              const result = await commands.shortcutCreate(pkg.package).catch(() => null);
              if (result?.status === "ok")
                toast({
                  tone: "success",
                  title: t("library.toasts.shortcutCreated"),
                  description: result.data,
                });
              else toast({ tone: "danger", title: t("library.toasts.shortcutFailed") });
            },
          },
        );
      }
      out.push(
        {
          id: "open-folder",
          label: t("library.actions.openFolder"),
          icon: "folder",
          disabled: isOffline(pkg, libraries),
          onSelect: async () => {
            const result = await commands.installOpenFolder(pkg.package).catch(() => null);
            if (result?.status !== "ok")
              toast({ tone: "danger", title: t("library.toasts.openFolderFailed") });
          },
        },
        { id: "sep-danger", separator: true },
        {
          id: "uninstall",
          label:
            pkg.state === "incomplete"
              ? t("library.actions.remove")
              : t("library.actions.uninstall"),
          icon: "trash",
          danger: true,
          disabled: pkg.running || (pkg.state !== "installed" && pkg.state !== "incomplete"),
          onSelect: () => setDialog({ kind: "uninstall", pkg }),
        },
      );
      return out;
    },
    [launch, libraries, navigate, run, toast, toggleFavorite, verify],
  );

  const value = useMemo<LibraryActions>(
    () => ({
      primary,
      runPrimary,
      entries,
      launching,
      openDetails: (pkg) => navigate(`/package/${pkg.package.package_id}`),
      openCollections: () => setDialog({ kind: "collections" }),
    }),
    [entries, launching, navigate, primary, runPrimary],
  );

  return (
    <LibraryActionsContext.Provider value={value}>
      {children}
      {dialog.kind === "collections" ? (
        <ManageCollectionsDialog collections={collections} installs={installs} onClose={close} />
      ) : null}
      {dialog.kind === "add_to_collection" ? (
        <AddToCollectionDialog pkg={dialog.pkg} collections={collections} onClose={close} />
      ) : null}
      {dialog.kind === "move" ? (
        <MoveDialog pkg={dialog.pkg} libraries={libraries} onClose={close} />
      ) : null}
      {dialog.kind === "uninstall" ? <UninstallDialog pkg={dialog.pkg} onClose={close} /> : null}
      <ConfirmDialog
        open={dialog.kind === "stop"}
        tone="danger"
        title={t("library.stop.title", { title: dialog.kind === "stop" ? dialog.pkg.title : "" })}
        description={t("library.stop.text")}
        confirmLabel={t("library.stop.confirm")}
        onCancel={close}
        onConfirm={async () => {
          if (dialog.kind !== "stop") return;
          const pkg = dialog.pkg;
          const result = await commands.gameStop(pkg.package).catch(() => null);
          close();
          if (result?.status === "ok") await refresh();
          else
            toast({ tone: "danger", title: t("library.toasts.stopFailed", { title: pkg.title }) });
        }}
      />
      <ConfirmDialog
        open={dialog.kind === "yanked_update"}
        title={
          dialog.kind === "yanked_update"
            ? t("library.yanked.title", {
                title: dialog.pkg.title,
                version: dialog.pkg.update?.version_label ?? "",
              })
            : ""
        }
        description={t("library.yanked.text")}
        confirmLabel={t("library.yanked.confirm", {
          version: dialog.kind === "yanked_update" ? (dialog.pkg.update?.version_label ?? "") : "",
        })}
        onCancel={close}
        onConfirm={async () => {
          if (dialog.kind !== "yanked_update") return;
          const pkg = dialog.pkg;
          close();
          await run(pkg, () => commands.installUpdate(pkg.package), {
            title: t("library.toasts.updateQueued", { title: pkg.title }),
            downloads: true,
          });
        }}
      />
    </LibraryActionsContext.Provider>
  );
}
