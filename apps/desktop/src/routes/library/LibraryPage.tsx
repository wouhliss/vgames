// Library (home): installed packages with favorites pinned at the top, collections as tabs, search,
// sort, grid/list layouts and every per-package action.
import { useQueryClient } from "@tanstack/react-query";
import { type DragEvent, type ReactNode, useDeferredValue, useMemo, useState } from "react";
import { useNavigate } from "react-router";
import { useCollections, useInstalls, useLibraries } from "../../app/queries";
import { oneOf, useStoredState } from "../../app/storage";
import { Button, IconButton } from "../../components/Button";
import { EmptyState, ErrorState, LoadingState } from "../../components/Feedback";
import { Icon } from "../../components/Icon";
import { Select } from "../../components/Select";
import { type TabItem, Tabs } from "../../components/Tabs";
import { TextField } from "../../components/TextField";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import { type Collection, commands, type InstalledPackage, type PackageRef } from "../../ipc";
import { queryKeys } from "../../ipc/query";
import { Page } from "../Page";
import { LibraryActionsProvider, useLibraryActionsContext } from "./actions";
import styles from "./Library.module.css";
import { LibraryView } from "./LibraryView";
import { collectionErrorMessage } from "./messages";
import {
  type FilterValue,
  filterInstalls,
  type SortKey,
  sectionsOf,
  sortInstalls,
  type ViewMode,
} from "./model";
import { PACKAGE_DRAG_TYPE } from "./Tile";

const SORTS: SortKey[] = ["recent", "name", "size", "installed"];

function parseDraggedPackage(e: DragEvent): PackageRef | null {
  try {
    const value: unknown = JSON.parse(e.dataTransfer.getData(PACKAGE_DRAG_TYPE));
    if (
      value !== null &&
      typeof value === "object" &&
      "server_id" in value &&
      "package_id" in value &&
      typeof value.server_id === "string" &&
      typeof value.package_id === "string"
    )
      return { server_id: value.server_id, package_id: value.package_id };
  } catch {
    // Not a package drag.
  }
  return null;
}

export function LibraryPage() {
  const installs = useInstalls();
  const collections = useCollections();
  const libraries = useLibraries();

  let body: ReactNode;
  // A failed refresh keeps showing the last good list; the error screen is only for a first load.
  if (installs.data === undefined || collections.data === undefined) {
    body =
      installs.isError || collections.isError ? (
        <ErrorState
          title={t("library.loadError")}
          onRetry={() => {
            void installs.refetch();
            void collections.refetch();
          }}
        />
      ) : (
        <LoadingState />
      );
  } else {
    body = (
      <LibraryActionsProvider
        libraries={libraries.data ?? []}
        collections={collections.data}
        installs={installs.data}
      >
        <LibraryContent installs={installs.data} collections={collections.data} />
      </LibraryActionsProvider>
    );
  }
  // One stable page (and heading) across loading, error and content.
  return <Page title={t("library.title")}>{body}</Page>;
}

function LibraryContent({
  installs,
  collections,
}: {
  installs: readonly InstalledPackage[];
  collections: readonly Collection[];
}) {
  const navigate = useNavigate();
  const actions = useLibraryActionsContext();
  const libraries = useLibraries();
  const client = useQueryClient();
  const { toast } = useToast();
  const [view, setView] = useStoredState<ViewMode>(
    "vgames.library.view",
    "grid",
    oneOf("grid", "list"),
  );
  const [sort, setSort] = useStoredState<SortKey>("vgames.library.sort", "recent", oneOf(...SORTS));
  const [filter, setFilter] = useState<FilterValue>("all");
  const [query, setQuery] = useState("");
  const deferredQuery = useDeferredValue(query);
  const [dropTarget, setDropTarget] = useState<string | null>(null);

  // A deleted collection's tab disappears; fall back to All.
  const activeFilter: FilterValue =
    filter.startsWith("c:") && !collections.some((c) => `c:${c.id}` === filter) ? "all" : filter;

  const sections = useMemo(
    () =>
      sectionsOf(
        sortInstalls(filterInstalls(installs, deferredQuery, activeFilter), sort),
        activeFilter,
      ),
    [installs, deferredQuery, activeFilter, sort],
  );
  const shown = sections.reduce((n, s) => n + s.items.length, 0);

  const dropOnCollection = async (e: DragEvent, collection: Collection) => {
    e.preventDefault();
    setDropTarget(null);
    const ref = parseDraggedPackage(e);
    if (!ref) return;
    const pkg = installs.find((i) => i.package.package_id === ref.package_id);
    try {
      const result = await commands.collectionAddPackage(collection.id, ref);
      if (result.status === "error") {
        toast({ tone: "danger", title: collectionErrorMessage(result.error) });
        return;
      }
      await client.invalidateQueries({ queryKey: queryKeys.installs });
      toast({
        tone: "success",
        title: t("library.collections.added", { title: pkg?.title ?? "", name: collection.name }),
      });
    } catch {
      toast({ tone: "danger", title: t("library.collections.errors.generic") });
    }
  };

  const count = (predicate: (pkg: InstalledPackage) => boolean) =>
    installs.reduce((n, pkg) => (predicate(pkg) ? n + 1 : n), 0);
  const tabs: TabItem<FilterValue>[] = [
    { value: "all", label: <TabLabel text={t("library.all")} count={installs.length} /> },
    {
      value: "favorites",
      label: <TabLabel text={t("library.favorites")} count={count((p) => p.favorite)} />,
    },
    ...[...collections]
      .sort((a, b) => a.position - b.position)
      .map(
        (collection): TabItem<FilterValue> => ({
          value: `c:${collection.id}`,
          label: (
            <TabLabel
              text={collection.name}
              count={count((p) => p.collection_ids.includes(collection.id))}
            />
          ),
          drop: {
            active: dropTarget === collection.id,
            onDragOver: (e) => {
              if (!e.dataTransfer.types.includes(PACKAGE_DRAG_TYPE)) return;
              e.preventDefault();
              e.dataTransfer.dropEffect = "copy";
              setDropTarget(collection.id);
            },
            onDragLeave: () => setDropTarget(null),
            onDrop: (e) => void dropOnCollection(e, collection),
          },
        }),
      ),
  ];

  if (installs.length === 0) {
    return (
      <EmptyState
        icon="library"
        title={t("library.emptyTitle")}
        description={t("library.emptyText")}
        action={
          <Button variant="primary" icon="browse" onClick={() => navigate("/browse")}>
            {t("library.emptyAction")}
          </Button>
        }
      />
    );
  }

  return (
    <>
      <div className={styles.toolbar} data-nav-group="">
        <div className={styles.search}>
          <TextField
            type="search"
            label={t("library.searchLabel")}
            hideLabel
            placeholder={t("library.searchPlaceholder")}
            prefix={<Icon name="search" size={18} />}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape" && query !== "") {
                e.preventDefault();
                e.stopPropagation();
                setQuery("");
              }
            }}
            autoComplete="off"
            spellCheck={false}
          />
        </div>
        <div className={styles.sort}>
          <Select
            label={t("library.sortLabel")}
            hideLabel
            value={sort}
            options={SORTS.map((value) => ({ value, label: t(`library.sort.${value}`) }))}
            onChange={setSort}
          />
        </div>
        <fieldset className={styles.viewToggle}>
          <legend className="visually-hidden">{t("library.viewLabel")}</legend>
          <IconButton
            icon="grid"
            label={t("library.viewGrid")}
            aria-pressed={view === "grid"}
            onClick={() => setView("grid")}
          />
          <IconButton
            icon="list"
            label={t("library.viewList")}
            aria-pressed={view === "list"}
            onClick={() => setView("list")}
          />
        </fieldset>
        <Button icon="collection" onClick={actions.openCollections}>
          {t("library.manageCollections")}
        </Button>
      </div>
      <Tabs label={t("library.filterLabel")} items={tabs} value={activeFilter} onChange={setFilter}>
        <p className="visually-hidden" role="status">
          {t("library.count", { count: shown })}
        </p>
        {shown === 0 ? (
          <FilteredEmpty filter={activeFilter} query={deferredQuery} onClear={() => setQuery("")} />
        ) : (
          <LibraryView sections={sections} view={view} libraries={libraries.data ?? []} />
        )}
      </Tabs>
    </>
  );
}

function TabLabel({ text, count }: { text: string; count: number }) {
  return (
    <>
      <span className={styles.tabText}>{text}</span>{" "}
      <span className={styles.tabCount}>{count}</span>
    </>
  );
}

function FilteredEmpty({
  filter,
  query,
  onClear,
}: {
  filter: FilterValue;
  query: string;
  onClear: () => void;
}) {
  if (query.trim() !== "") {
    return (
      <EmptyState
        icon="search"
        title={t("library.noMatchTitle", { query: query.trim() })}
        description={t("library.noMatchText")}
        action={<Button onClick={onClear}>{t("library.clearSearch")}</Button>}
      />
    );
  }
  if (filter === "favorites") {
    return (
      <EmptyState
        icon="star"
        title={t("library.emptyFavoritesTitle")}
        description={t("library.emptyFavoritesText")}
      />
    );
  }
  return (
    <EmptyState
      icon="collection"
      title={t("library.emptyCollectionTitle")}
      description={t("library.emptyCollectionText")}
    />
  );
}
