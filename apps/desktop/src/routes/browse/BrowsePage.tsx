// Browse: the server's catalog with search (debounced), genre filter and sort, shown as a
// virtualized grid that loads pages as the player scrolls.
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { type ReactNode, useCallback, useMemo, useState } from "react";
import { useDebouncedValue } from "../../app/debounce";
import { useInstalls } from "../../app/queries";
import { oneOf, useStoredState } from "../../app/storage";
import { Button } from "../../components/Button";
import { EmptyState, ErrorState, LoadingState } from "../../components/Feedback";
import { Icon } from "../../components/Icon";
import { Select } from "../../components/Select";
import { TextField } from "../../components/TextField";
import { t } from "../../i18n";
import { type CatalogSort, commands } from "../../ipc";
import { queryKeys, unwrap } from "../../ipc/query";
import { Page } from "../Page";
import styles from "./Browse.module.css";
import { CatalogGrid } from "./CatalogGrid";

const ALL = "__all__";

export function BrowsePage() {
  const [query, setQuery] = useState("");
  const [genre, setGenre] = useState<string>(ALL);
  const [sort, setSort] = useStoredState<CatalogSort>(
    "vgames.browse.sort",
    "title",
    oneOf("title", "recent"),
  );
  const debounced = useDebouncedValue(query.trim(), 300);
  const installs = useInstalls();
  const genres = useQuery({
    queryKey: queryKeys.genres,
    queryFn: async () => unwrap(await commands.catalogGenres()),
  });
  const filters = { query: debounced, genre: genre === ALL ? null : genre, sort };
  const catalog = useInfiniteQuery({
    queryKey: [...queryKeys.catalog, filters],
    queryFn: async ({ pageParam }) =>
      unwrap(await commands.catalogList({ ...filters, cursor: pageParam })),
    initialPageParam: null as string | null,
    getNextPageParam: (last) => last.next_cursor,
  });

  const items = useMemo(() => catalog.data?.pages.flatMap((p) => p.items) ?? [], [catalog.data]);
  const installedIds = useMemo(
    () => new Set((installs.data ?? []).map((i) => i.package.package_id)),
    [installs.data],
  );
  const { fetchNextPage } = catalog;
  const loadMore = useCallback(() => void fetchNextPage(), [fetchNextPage]);
  const filtered = debounced !== "" || genre !== ALL;
  const clear = () => {
    setQuery("");
    setGenre(ALL);
  };

  let body: ReactNode;
  if (catalog.isPending) body = <LoadingState />;
  else if (catalog.isError && items.length === 0)
    body = (
      <ErrorState
        title={t("browse.loadError")}
        description={t("browse.loadErrorText")}
        onRetry={() => void catalog.refetch()}
      />
    );
  else if (items.length === 0)
    body = filtered ? (
      <EmptyState
        icon="search"
        title={t("browse.noMatchTitle")}
        description={t("browse.noMatchText")}
        action={<Button onClick={clear}>{t("browse.clearFilters")}</Button>}
      />
    ) : (
      <EmptyState
        icon="browse"
        title={t("browse.emptyTitle")}
        description={t("browse.emptyText")}
      />
    );
  else
    body = (
      <CatalogGrid
        items={items}
        installedIds={installedIds}
        hasMore={catalog.hasNextPage}
        loadingMore={catalog.isFetchingNextPage}
        loadMoreFailed={catalog.isFetchNextPageError}
        onLoadMore={loadMore}
      />
    );

  return (
    <Page title={t("browse.title")}>
      <div className={styles.toolbar} data-nav-group="">
        <div className={styles.search}>
          <TextField
            type="search"
            label={t("browse.searchLabel")}
            hideLabel
            placeholder={t("browse.searchPlaceholder")}
            prefix={<Icon name="search" size={18} />}
            value={query}
            maxLength={100}
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
        <div className={styles.filter}>
          <Select
            label={t("browse.genreLabel")}
            hideLabel
            value={genre}
            options={[
              { value: ALL, label: t("browse.allGenres") },
              ...(genres.data ?? []).map((g) => ({
                value: g.genre,
                label: t("browse.genreOption", { genre: g.genre, count: g.count }),
              })),
            ]}
            onChange={setGenre}
          />
        </div>
        <div className={styles.filter}>
          <Select
            label={t("browse.sortLabel")}
            hideLabel
            value={sort}
            options={[
              { value: "title", label: t("browse.sort.title") },
              { value: "recent", label: t("browse.sort.recent") },
            ]}
            onChange={setSort}
          />
        </div>
      </div>
      <p className="visually-hidden" role="status">
        {catalog.isPending ? "" : t("browse.count", { count: items.length })}
      </p>
      {body}
    </Page>
  );
}
