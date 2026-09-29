// Pieces the server pages share: the signed-in role, URL-kept filters and a cursor-paged table footer.
import type { UseInfiniteQueryResult } from "@tanstack/react-query";
import { useSearchParams } from "react-router";
import { ErrorView } from "../../app/ErrorView";
import { useMe } from "../../app/session";

export function useRole(): "user" | "admin" | "owner" {
  return useMe().data?.user.role ?? "user";
}

/** Filters kept in the URL (back/forward and deep links work). */
export function useUrlFilters<K extends string>(keys: readonly K[]) {
  const [params, setParams] = useSearchParams();
  const values = Object.fromEntries(keys.map((k) => [k, params.get(k) ?? ""])) as Record<K, string>;
  const apply = (next: Record<K, string>) => {
    const out = new URLSearchParams();
    for (const k of keys) if (next[k].trim()) out.set(k, next[k].trim());
    setParams(out);
  };
  return { values, apply };
}

export function LoadMore({
  query,
  count,
}: {
  query: UseInfiniteQueryResult<unknown>;
  count: number;
}) {
  return (
    <>
      <div className="row" style={{ marginTop: 8 }}>
        <span className="muted" role="status">
          {count.toLocaleString("en")} shown{query.hasNextPage ? ", more available" : ""}
        </span>
        {query.hasNextPage ? (
          <button
            type="button"
            disabled={query.isFetchingNextPage}
            onClick={() => void query.fetchNextPage()}
          >
            {query.isFetchingNextPage ? "Loading…" : "Load more"}
          </button>
        ) : null}
      </div>
      {query.isFetchNextPageError ? (
        <ErrorView error={query.error} onRetry={() => void query.fetchNextPage()} />
      ) : null}
    </>
  );
}

export const when = (iso: string | undefined) => (iso ? new Date(iso).toLocaleString() : "—");
