// Library dialogs: add to collections, manage collections, move to another library, uninstall.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type DragEvent, type FormEvent, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { useInstalls } from "../../app/queries";
import { Button, IconButton } from "../../components/Button";
import { Checkbox } from "../../components/Checkbox";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { Dialog } from "../../components/Dialog";
import { LoadingState } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { RadioGroup, type RadioOption } from "../../components/RadioGroup";
import { TextField } from "../../components/TextField";
import { useToast } from "../../components/Toast";
import { formatBytes, t } from "../../i18n";
import {
  type Collection,
  type CollectionError,
  commands,
  type InstalledPackage,
  type LibraryInfo,
  type Result,
} from "../../ipc";
import { queryKeys } from "../../ipc/query";
import styles from "./Library.module.css";
import { actionErrorMessage, collectionErrorMessage } from "./messages";

const COLLECTION_DRAG_TYPE = "application/x-vgames-collection";

function useCollectionMutation() {
  const client = useQueryClient();
  return async <T,>(action: () => Promise<Result<T, CollectionError>>): Promise<string | null> => {
    try {
      const result = await action();
      await Promise.all([
        client.invalidateQueries({ queryKey: queryKeys.collections }),
        client.invalidateQueries({ queryKey: queryKeys.installs }),
      ]);
      return result.status === "ok" ? null : collectionErrorMessage(result.error);
    } catch {
      return t("library.collections.errors.generic");
    }
  };
}

/** "New collection" field with a Create button. Calls `onCreated` with the new collection. */
function CreateCollectionForm({ onCreated }: { onCreated?: (c: Collection) => Promise<void> }) {
  const client = useQueryClient();
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    const trimmed = name.trim();
    if (trimmed === "" || [...trimmed].length > 100) {
      setError(t("library.collections.errors.invalidName"));
      return;
    }
    setBusy(true);
    try {
      const result = await commands.collectionCreate(trimmed);
      if (result.status === "error") {
        setError(collectionErrorMessage(result.error));
        return;
      }
      setName("");
      setError(null);
      await onCreated?.(result.data);
      await client.invalidateQueries({ queryKey: queryKeys.collections });
    } catch {
      setError(t("library.collections.errors.generic"));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className={styles.inlineForm} onSubmit={submit}>
      <TextField
        label={t("library.collections.newLabel")}
        value={name}
        maxLength={100}
        onChange={(e) => {
          setName(e.target.value);
          if (error) setError(null);
        }}
        error={error}
        autoComplete="off"
      />
      <Button type="submit" icon="plus" loading={busy}>
        {t("library.collections.create")}
      </Button>
    </form>
  );
}

export function AddToCollectionDialog({
  pkg,
  collections,
  onClose,
}: {
  pkg: InstalledPackage;
  collections: readonly Collection[];
  onClose: () => void;
}) {
  const installs = useInstalls();
  const mutate = useCollectionMutation();
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Follow the live install so checkboxes reflect what the core stored.
  const live = installs.data?.find((i) => i.package.package_id === pkg.package.package_id) ?? pkg;

  const toggle = async (collection: Collection, member: boolean) => {
    setBusy(collection.id);
    const message = await mutate(() =>
      member
        ? commands.collectionAddPackage(collection.id, live.package)
        : commands.collectionRemovePackage(collection.id, live.package),
    );
    setError(message);
    setBusy(null);
  };

  return (
    <Dialog
      open
      title={t("library.collections.addTitle", { title: pkg.title })}
      onClose={onClose}
      footer={
        <Button variant="primary" onClick={onClose}>
          {t("common.done")}
        </Button>
      }
    >
      <div className={styles.dialogStack}>
        {collections.length === 0 ? (
          <p className={styles.muted}>{t("library.collections.none")}</p>
        ) : (
          <fieldset className={styles.checkList} aria-busy={busy !== null || undefined}>
            <legend className="visually-hidden">{t("library.collections.title")}</legend>
            {collections.map((collection) => (
              <Checkbox
                key={collection.id}
                label={collection.name}
                checked={live.collection_ids.includes(collection.id)}
                // Never `disabled` while saving: that would drop keyboard and controller focus.
                onCheckedChange={(checked) => {
                  if (busy === null) void toggle(collection, checked);
                }}
              />
            ))}
          </fieldset>
        )}
        {error ? (
          <Notice tone="danger" role="alert">
            {error}
          </Notice>
        ) : null}
        <CreateCollectionForm
          onCreated={async (created) => {
            const message = await mutate(() =>
              commands.collectionAddPackage(created.id, live.package),
            );
            setError(message);
          }}
        />
      </div>
    </Dialog>
  );
}

export function ManageCollectionsDialog({
  collections,
  installs,
  onClose,
}: {
  collections: readonly Collection[];
  installs: readonly InstalledPackage[];
  onClose: () => void;
}) {
  const mutate = useCollectionMutation();
  const [editing, setEditing] = useState<{ id: string } | null>(null);
  const [deleting, setDeleting] = useState<Collection | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [dropTarget, setDropTarget] = useState<string | null>(null);

  const ordered = [...collections].sort((a, b) => a.position - b.position);
  const counts = new Map<string, number>();
  for (const pkg of installs)
    for (const id of pkg.collection_ids) counts.set(id, (counts.get(id) ?? 0) + 1);

  const reorder = async (ids: string[]) => {
    setError(await mutate(() => commands.collectionsReorder(ids)));
  };
  const move = (index: number, delta: -1 | 1) => {
    const ids = ordered.map((c) => c.id);
    const target = index + delta;
    const moved = ids[index];
    const other = ids[target];
    if (moved === undefined || other === undefined) return;
    ids[index] = other;
    ids[target] = moved;
    void reorder(ids);
  };
  const dropOn = (e: DragEvent<HTMLLIElement>, targetId: string) => {
    e.preventDefault();
    setDropTarget(null);
    const draggedId = e.dataTransfer.getData(COLLECTION_DRAG_TYPE);
    if (!draggedId || draggedId === targetId) return;
    const ids = ordered.map((c) => c.id).filter((id) => id !== draggedId);
    ids.splice(ids.indexOf(targetId), 0, draggedId);
    void reorder(ids);
  };

  return (
    <>
      <Dialog
        open
        size="lg"
        title={t("library.collections.title")}
        description={t("library.collections.text")}
        onClose={onClose}
        footer={
          <Button variant="primary" onClick={onClose}>
            {t("common.done")}
          </Button>
        }
      >
        <div className={styles.dialogStack}>
          <CreateCollectionForm />
          {error ? (
            <Notice tone="danger" role="alert">
              {error}
            </Notice>
          ) : null}
          {ordered.length === 0 ? (
            <p className={styles.muted}>{t("library.collections.none")}</p>
          ) : (
            <ol className={styles.collectionList}>
              {ordered.map((collection, index) => (
                <li
                  key={collection.id}
                  className={styles.collectionRow}
                  data-drop-active={dropTarget === collection.id ? "true" : undefined}
                  draggable={editing?.id !== collection.id}
                  onDragStart={(e) => {
                    e.dataTransfer.setData(COLLECTION_DRAG_TYPE, collection.id);
                    e.dataTransfer.effectAllowed = "move";
                  }}
                  onDragOver={(e) => {
                    if (!e.dataTransfer.types.includes(COLLECTION_DRAG_TYPE)) return;
                    e.preventDefault();
                    setDropTarget(collection.id);
                  }}
                  onDragLeave={() => setDropTarget(null)}
                  onDrop={(e) => dropOn(e, collection.id)}
                >
                  {editing?.id === collection.id ? (
                    <RenameForm
                      collection={collection}
                      onSave={async (name) => {
                        const message = await mutate(() =>
                          commands.collectionRename(collection.id, name),
                        );
                        setError(message);
                        if (message === null) setEditing(null);
                      }}
                      onCancel={() => setEditing(null)}
                    />
                  ) : (
                    <>
                      <span className={styles.collectionName}>{collection.name}</span>
                      <span className={styles.muted}>
                        {t("library.collections.packages", {
                          count: counts.get(collection.id) ?? 0,
                        })}
                      </span>
                      <span className={styles.rowActions}>
                        <IconButton
                          icon="edit"
                          size="sm"
                          label={t("library.collections.renameAction", { name: collection.name })}
                          onClick={() => setEditing({ id: collection.id })}
                        />
                        <IconButton
                          icon="arrowUp"
                          size="sm"
                          label={t("library.collections.moveUp", { name: collection.name })}
                          aria-disabled={index === 0 || undefined}
                          onClick={() => move(index, -1)}
                        />
                        <IconButton
                          icon="arrowDown"
                          size="sm"
                          label={t("library.collections.moveDown", { name: collection.name })}
                          aria-disabled={index === ordered.length - 1 || undefined}
                          onClick={() => move(index, 1)}
                        />
                        <IconButton
                          icon="trash"
                          size="sm"
                          label={t("library.collections.delete", { name: collection.name })}
                          onClick={() => setDeleting(collection)}
                        />
                      </span>
                    </>
                  )}
                </li>
              ))}
            </ol>
          )}
        </div>
      </Dialog>
      <ConfirmDialog
        open={deleting !== null}
        tone="danger"
        title={t("library.collections.deleteTitle", { name: deleting?.name ?? "" })}
        description={t("library.collections.deleteText")}
        confirmLabel={t("library.collections.deleteConfirm")}
        onCancel={() => setDeleting(null)}
        onConfirm={async () => {
          if (!deleting) return;
          setError(await mutate(() => commands.collectionDelete(deleting.id)));
          setDeleting(null);
        }}
      />
    </>
  );
}

function RenameForm({
  collection,
  onSave,
  onCancel,
}: {
  collection: Collection;
  onSave: (name: string) => Promise<void>;
  onCancel: () => void;
}) {
  const [name, setName] = useState(collection.name);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    input.current?.focus();
    input.current?.select();
  }, []);
  return (
    <form
      className={styles.inlineForm}
      onSubmit={(e) => {
        e.preventDefault();
        void onSave(name.trim());
      }}
    >
      <TextField
        ref={input}
        label={t("library.collections.renameLabel", { name: collection.name })}
        hideLabel
        value={name}
        maxLength={100}
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            onCancel();
          }
        }}
        autoComplete="off"
      />
      <Button type="submit" variant="primary" size="sm">
        {t("library.collections.save")}
      </Button>
      <Button size="sm" onClick={onCancel}>
        {t("common.cancel")}
      </Button>
    </form>
  );
}

export function MoveDialog({
  pkg,
  libraries,
  onClose,
}: {
  pkg: InstalledPackage;
  libraries: readonly LibraryInfo[];
  onClose: () => void;
}) {
  const navigate = useNavigate();
  const { toast } = useToast();
  const client = useQueryClient();
  const others = libraries.filter((l) => l.id !== pkg.library_id);
  const usable = (l: LibraryInfo) =>
    l.online && (l.free_bytes === null || l.free_bytes >= pkg.size_bytes);
  const [target, setTarget] = useState<string | null>(others.find(usable)?.id ?? null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const options: RadioOption<string>[] = others.map((library) => ({
    value: library.id,
    label: library.label ?? library.path,
    description: !library.online
      ? t("library.move.offline")
      : !usable(library)
        ? t("library.move.notEnough", { needed: formatBytes(pkg.size_bytes) })
        : library.label
          ? library.path
          : undefined,
    aside:
      library.online && library.free_bytes !== null
        ? t("library.move.free", { free: formatBytes(library.free_bytes) })
        : undefined,
    disabled: !usable(library),
  }));

  const confirm = async () => {
    if (!target || busy) return;
    setBusy(true);
    try {
      const result = await commands.installMove(pkg.package, target);
      if (result.status === "error") {
        setError(actionErrorMessage(result.error, pkg.title));
        return;
      }
      await client.invalidateQueries({ queryKey: queryKeys.installs });
      toast({ tone: "info", title: t("library.toasts.moveStarted", { title: pkg.title }) });
      onClose();
    } catch {
      setError(t("error.generic"));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open
      title={t("library.move.title", { title: pkg.title })}
      description={others.length > 0 ? t("library.move.text") : undefined}
      onClose={onClose}
      dismissible={!busy}
      footer={
        others.length > 0 ? (
          <>
            <Button onClick={onClose} aria-disabled={busy || undefined}>
              {t("common.cancel")}
            </Button>
            <Button
              variant="primary"
              loading={busy}
              aria-disabled={target === null || undefined}
              onClick={() => void confirm()}
            >
              {t("library.move.confirm")}
            </Button>
          </>
        ) : (
          <>
            <Button onClick={onClose}>{t("common.close")}</Button>
            <Button
              variant="primary"
              onClick={() => {
                onClose();
                navigate("/settings/storage");
              }}
            >
              {t("library.move.openSettings")}
            </Button>
          </>
        )
      }
    >
      <div className={styles.dialogStack}>
        {others.length === 0 ? (
          <p>{t("library.move.noOther")}</p>
        ) : (
          <RadioGroup
            label={t("library.move.target")}
            value={target}
            options={options}
            onChange={setTarget}
          />
        )}
        {error ? (
          <Notice tone="danger" role="alert">
            {error}
          </Notice>
        ) : null}
      </div>
    </Dialog>
  );
}

export function UninstallDialog({ pkg, onClose }: { pkg: InstalledPackage; onClose: () => void }) {
  const { toast } = useToast();
  const client = useQueryClient();
  const [removeLeftovers, setRemoveLeftovers] = useState(false);
  const [removePrefix, setRemovePrefix] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const plan = useQuery({
    queryKey: ["install_uninstall_plan", pkg.package.server_id, pkg.package.package_id],
    queryFn: async () => {
      const result = await commands.installUninstallPlan(pkg.package);
      if (result.status === "error") throw new Error(actionErrorMessage(result.error, pkg.title));
      return result.data;
    },
    gcTime: 0,
  });

  const confirm = async () => {
    setError(null);
    try {
      const result = await commands.installUninstall(pkg.package, removeLeftovers, removePrefix);
      if (result.status === "error") {
        setError(actionErrorMessage(result.error, pkg.title));
        return;
      }
      await client.invalidateQueries({ queryKey: queryKeys.installs });
      toast({ tone: "info", title: t("library.toasts.uninstallStarted", { title: pkg.title }) });
      onClose();
    } catch {
      setError(t("error.generic"));
    }
  };

  const data = plan.data;
  const hidden = data ? data.leftover_count - data.leftovers.length : 0;

  return (
    <ConfirmDialog
      open
      tone="danger"
      title={t("library.uninstall.title", { title: pkg.title })}
      description={data ? t("library.uninstall.text", { size: formatBytes(data.size_bytes) }) : ""}
      confirmLabel={t("library.uninstall.confirm")}
      onCancel={onClose}
      confirmDisabled={!data}
      onConfirm={confirm}
    >
      <div className={styles.dialogStack}>
        {plan.isPending ? <LoadingState label={t("library.uninstall.loading")} /> : null}
        {plan.isError ? (
          <Notice tone="danger" role="alert">
            {plan.error instanceof Error && plan.error.message
              ? plan.error.message
              : t("library.uninstall.loadError")}
          </Notice>
        ) : null}
        {data && data.leftover_count > 0 ? (
          <section className={styles.leftovers} aria-labelledby="leftovers-title">
            <h3 id="leftovers-title">{t("library.uninstall.leftoversTitle")}</h3>
            <p className={styles.muted}>
              {t("library.uninstall.leftoversText", {
                count: data.leftover_count,
                size: formatBytes(data.leftover_bytes),
              })}
            </p>
            <ul className={styles.fileList} data-selectable="">
              {data.leftovers.map((file) => (
                <li key={file.path}>
                  <span className={styles.filePath}>{file.path}</span>
                  <span className={styles.muted}>{formatBytes(file.size_bytes)}</span>
                </li>
              ))}
              {hidden > 0 ? (
                <li className={styles.muted}>{t("library.uninstall.more", { count: hidden })}</li>
              ) : null}
            </ul>
            <Checkbox
              label={t("library.uninstall.deleteLeftovers")}
              description={t("library.uninstall.keepHint")}
              checked={removeLeftovers}
              onCheckedChange={setRemoveLeftovers}
            />
          </section>
        ) : null}
        {data?.has_prefix ? (
          <Checkbox
            label={t("library.uninstall.prefix")}
            description={t("library.uninstall.prefixHint")}
            checked={removePrefix}
            onCheckedChange={setRemovePrefix}
          />
        ) : null}
        {error ? (
          <Notice tone="danger" role="alert">
            {error}
          </Notice>
        ) : null}
      </div>
    </ConfirmDialog>
  );
}
