import { useInfiniteQuery } from "@tanstack/react-query";
import { compatHistory, compatKeys } from "../../api/compat";
import { ErrorView, Loading } from "../../app/ErrorView";

/** Signed revisions are read-only; the current editor remains separate from the history. */
export function CompatHistory({
  packageId,
  target,
}: {
  packageId: string;
  target: "linux" | "macos";
}) {
  const history = useInfiniteQuery({
    queryKey: compatKeys.history(packageId, target),
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam, signal }) =>
      (await compatHistory(packageId, target, pageParam, signal)).data,
    getNextPageParam: (page) => page.next_cursor,
  });
  const title = `${target}-history`;
  return (
    <section aria-labelledby={title}>
      <h3 id={title}>Revision history</h3>
      {history.isPending ? <Loading label="Loading revision history…" /> : null}
      {history.isError ? (
        <ErrorView error={history.error} onRetry={() => void history.refetch()} />
      ) : null}
      {history.data ? (
        <>
          {history.data.pages[0]?.items.length === 0 ? (
            <p>No revisions yet.</p>
          ) : (
            <ol>
              {history.data.pages
                .flatMap((page) => page.items)
                .map((item) => (
                  <li key={item.revision}>
                    Revision {item.revision}: {item.status}. Signed by key{" "}
                    <span className="mono">{item.signature.key_id}</span> on{" "}
                    <time dateTime={item.created_at}>
                      {new Date(item.created_at).toLocaleString()}
                    </time>
                    .
                  </li>
                ))}
            </ol>
          )}
          {history.hasNextPage ? (
            <button
              type="button"
              disabled={history.isFetchingNextPage}
              onClick={() => void history.fetchNextPage()}
            >
              {history.isFetchingNextPage ? "Loading earlier revisions…" : "Load earlier revisions"}
            </button>
          ) : null}
        </>
      ) : null}
    </section>
  );
}
