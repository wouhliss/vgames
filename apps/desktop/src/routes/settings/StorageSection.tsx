// Storage: libraries with free space, the default one, adding and removing libraries, and moving
// every package out of a library (one `install_move` per package).
import { useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useState } from "react";
import { useInstalls, useLibraries } from "../../app/queries";
import { Button } from "../../components/Button";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { Dialog } from "../../components/Dialog";
import { Badge, ErrorState, LoadingState } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { ProgressBar } from "../../components/ProgressBar";
import { RadioGroup } from "../../components/RadioGroup";
import { useToast } from "../../components/Toast";
import { formatBytes, t } from "../../i18n";
import { commands, type InstalledPackage, type Library, type LibraryRemoveError } from "../../ipc";
import { queryKeys } from "../../ipc/query";
import { libraryErrorMessage } from "../onboarding/messages";
import styles from "./Settings.module.css";
import { Section } from "./SettingsPage";

function removeErrorMessage(error: LibraryRemoveError): string {
  if (error.kind === "not_empty")
    return t("settings.storage.errors.not_empty", { count: error.install_count });
  if (error.kind === "is_default") return t("settings.storage.errors.is_default");
  return libraryErrorMessage(error);
}

function MoveAllDialog({
  source,
  libraries,
  installs,
  onClose,
}: {
  source: Library;
  libraries: readonly Library[];
  installs: readonly InstalledPackage[];
  onClose: () => void;
}) {
  const client = useQueryClient();
  const { toast } = useToast();
  const needed = installs.reduce((sum, i) => sum + i.size_bytes, 0);
  const targets = libraries.filter((l) => l.id !== source.id);
  const fits = (l: Library) => l.online && (l.free_bytes === null || l.free_bytes >= needed);
  const [target, setTarget] = useState<string | null>(targets.find(fits)?.id ?? null);
  const [busy, setBusy] = useState(false);

  const move = async () => {
    if (!target) return;
    setBusy(true);
    let failed = 0;
    for (const pkg of installs) {
      const result = await commands.installMove(pkg.package, target).catch(() => null);
      if (result?.status !== "ok") failed += 1;
    }
    setBusy(false);
    await client.invalidateQueries({ queryKey: queryKeys.installs });
    await client.invalidateQueries({ queryKey: queryKeys.libraries });
    const moved = installs.length - failed;
    if (moved > 0)
      toast({ tone: "success", title: t("settings.storage.moveStarted", { count: moved }) });
    if (failed > 0)
      toast({ tone: "danger", title: t("settings.storage.moveFailed", { count: failed }) });
    onClose();
  };

  return (
    <Dialog
      open
      title={t("settings.storage.moveDialogTitle", { count: installs.length, path: source.path })}
      description={t("settings.storage.moveDialogText")}
      onClose={onClose}
      dismissible={!busy}
      footer={
        <>
          <Button onClick={onClose} aria-disabled={busy || undefined}>
            {t("common.cancel")}
          </Button>
          <Button
            variant="primary"
            loading={busy}
            aria-disabled={target === null || undefined}
            onClick={() => void move()}
          >
            {t("settings.storage.moveConfirm")}
          </Button>
        </>
      }
    >
      <RadioGroup
        label={t("settings.storage.moveTo")}
        value={target}
        onChange={setTarget}
        options={targets.map((l) => ({
          value: l.id,
          label: l.label ?? l.path,
          description: !l.online
            ? t("settings.storage.offline")
            : !fits(l)
              ? t("settings.storage.moveNotEnough", { needed: formatBytes(needed) })
              : undefined,
          aside:
            l.online && l.free_bytes !== null
              ? t("install.free", { free: formatBytes(l.free_bytes) })
              : undefined,
          disabled: !fits(l),
        }))}
      />
    </Dialog>
  );
}

export function StorageSection() {
  const libraries = useLibraries();
  const installs = useInstalls();
  const client = useQueryClient();
  const { toast } = useToast();
  const [removing, setRemoving] = useState<Library | null>(null);
  const [moving, setMoving] = useState<Library | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const refresh = () => client.invalidateQueries({ queryKey: queryKeys.libraries });

  const makeDefault = async (library: Library) => {
    setBusy(library.id);
    setError(null);
    const result = await commands.librarySetDefault(library.id).catch(() => null);
    setBusy(null);
    if (result?.status === "ok") {
      await refresh();
      toast({
        tone: "success",
        title: t("settings.storage.defaultChanged", { path: library.path }),
      });
    } else
      setError(
        result?.status === "error"
          ? libraryErrorMessage(result.error)
          : t("settings.storage.errors.failed"),
      );
  };

  const remove = async (library: Library) => {
    setError(null);
    const result = await commands.libraryRemove(library.id).catch(() => null);
    setRemoving(null);
    if (result?.status === "ok") {
      await refresh();
      toast({ tone: "success", title: t("settings.storage.removed", { path: library.path }) });
    } else
      setError(
        result?.status === "error"
          ? removeErrorMessage(result.error)
          : t("settings.storage.errors.failed"),
      );
  };

  const add = async () => {
    setError(null);
    setBusy("add");
    try {
      const pick = await commands.libraryPickFolder();
      if (pick.status === "error") {
        setError(
          t("onboarding.library.errors.io", {
            detail: "detail" in pick.error ? pick.error.detail : pick.error.kind,
          }),
        );
        return;
      }
      if (!pick.data) return;
      const result = await commands.libraryAdd(pick.data.path, false);
      if (result.status === "error") {
        setError(libraryErrorMessage(result.error));
        return;
      }
      await refresh();
      toast({ tone: "success", title: t("settings.storage.added", { path: result.data.path }) });
    } catch {
      setError(t("settings.storage.errors.failed"));
    } finally {
      setBusy(null);
    }
  };

  const installsIn = (library: Library) =>
    (installs.data ?? []).filter((i) => i.library_id === library.id);

  let body: ReactNode;
  if (libraries.data) {
    const list = libraries.data;
    body = (
      <ul className={styles.cards} data-nav-group="">
        {list.map((library) => {
          const count = installsIn(library).length;
          const used =
            library.free_bytes !== null && library.total_bytes
              ? 1 - library.free_bytes / library.total_bytes
              : null;
          return (
            <li key={library.id} className={styles.card} aria-labelledby={`library-${library.id}`}>
              <div className={styles.cardHeader}>
                <h3 id={`library-${library.id}`} data-selectable="">
                  {library.label ?? library.path}
                </h3>
                {library.is_default ? (
                  <Badge tone="accent">{t("settings.storage.default")}</Badge>
                ) : null}
                {library.online ? null : (
                  <Badge tone="warning" icon="offline">
                    {t("settings.storage.offline")}
                  </Badge>
                )}
              </div>
              {library.label ? <p className={styles.muted}>{library.path}</p> : null}
              {used !== null && library.free_bytes !== null && library.total_bytes ? (
                <ProgressBar
                  label={t("settings.storage.spaceLabel", { path: library.path })}
                  hideLabel
                  size="sm"
                  value={used}
                  valueText={t("settings.storage.space", {
                    free: formatBytes(library.free_bytes),
                    total: formatBytes(library.total_bytes),
                  })}
                />
              ) : null}
              <p className={styles.muted}>
                {library.free_bytes !== null && library.total_bytes
                  ? `${t("settings.storage.space", {
                      free: formatBytes(library.free_bytes),
                      total: formatBytes(library.total_bytes),
                    })} · `
                  : ""}
                {t("settings.storage.installs", { count })}
              </p>
              <div className={styles.actions}>
                {library.is_default || !library.online ? null : (
                  <Button
                    size="sm"
                    loading={busy === library.id}
                    aria-label={t("settings.storage.makeDefaultTitle", { path: library.path })}
                    onClick={() => void makeDefault(library)}
                  >
                    {t("settings.storage.makeDefault")}
                  </Button>
                )}
                {count > 0 && list.length > 1 && library.online ? (
                  <Button
                    size="sm"
                    aria-label={t("settings.storage.moveAllTitle", { path: library.path })}
                    onClick={() => setMoving(library)}
                  >
                    {t("settings.storage.moveAll")}
                  </Button>
                ) : null}
                <Button
                  size="sm"
                  variant="ghost"
                  icon="trash"
                  aria-label={t("settings.storage.removeTitle", { path: library.path })}
                  onClick={() => setRemoving(library)}
                >
                  {t("settings.storage.remove")}
                </Button>
              </div>
            </li>
          );
        })}
      </ul>
    );
  } else if (libraries.isError) {
    body = <ErrorState title={t("error.generic")} onRetry={() => void libraries.refetch()} />;
  } else body = <LoadingState />;

  return (
    <Section id="storage" title={t("settings.section.storage")} text={t("settings.storage.text")}>
      {error ? (
        <Notice tone="danger" role="alert">
          {error}
        </Notice>
      ) : null}
      {body}
      <div>
        <Button icon="plus" loading={busy === "add"} onClick={() => void add()}>
          {t("settings.storage.add")}
        </Button>
      </div>
      <ConfirmDialog
        open={removing !== null}
        tone="danger"
        title={t("settings.storage.removeConfirmTitle", { path: removing?.path ?? "" })}
        description={t("settings.storage.removeConfirmText")}
        confirmLabel={t("settings.storage.removeConfirm")}
        onCancel={() => setRemoving(null)}
        onConfirm={() => (removing ? remove(removing) : undefined)}
      />
      {moving ? (
        <MoveAllDialog
          source={moving}
          libraries={libraries.data ?? []}
          installs={installsIn(moving)}
          onClose={() => setMoving(null)}
        />
      ) : null}
    </Section>
  );
}
