// Packages: filter by status and text (kept in the URL, so back/forward and links work), a table
// with sticky headers, and cursor paging with "Load more".
import { useInfiniteQuery } from "@tanstack/react-query";
import { type FormEvent, useEffect, useState } from "react";
import { Link, useLocation, useSearchParams } from "react-router";
import { ApiError } from "../../api/errors";
import { listPackages, type PackageFilters, packageKeys } from "../../api/packages";
import { type PackageStatus, PackageStatusSchema } from "../../api/schemas";
import { ErrorView, ForbiddenPage, Loading } from "../../app/ErrorView";
import { SpacerRow, useTableWindow } from "../../components/tableWindow";
import { length, STATUS_LABEL, STATUSES } from "./model";

function readFilters(params: URLSearchParams): PackageFilters {
  const status = PackageStatusSchema.safeParse(params.get("status"));
  return { q: params.get("q") ?? "", status: status.success ? status.data : "" };
}

export function PackagesList() {
  const [params, setParams] = useSearchParams();
  const location = useLocation();
  const deleted =
    typeof location.state === "object" && location.state !== null && "deleted" in location.state
      ? String(location.state.deleted)
      : null;
  const filters = readFilters(params);
  const [q, setQ] = useState(filters.q);
  const [status, setStatus] = useState<PackageStatus | "">(filters.status);
  const tooLong = length(q.trim()) > 100;
  // Back/forward and links change the URL: the form follows.
  useEffect(() => {
    setQ(filters.q);
    setStatus(filters.status);
  }, [filters.q, filters.status]);

  const pages = useInfiniteQuery({
    queryKey: packageKeys.list(filters),
    queryFn: async ({ pageParam, signal }) => (await listPackages(filters, pageParam, signal)).data,
    initialPageParam: null as string | null,
    getNextPageParam: (last) => last.next_cursor ?? null,
  });

  const apply = (e: FormEvent) => {
    e.preventDefault();
    if (tooLong) return;
    const next = new URLSearchParams();
    if (q.trim()) next.set("q", q.trim());
    if (status) next.set("status", status);
    setParams(next);
  };

  const items = pages.data?.pages.flatMap((p) => p.items) ?? [];
  const win = useTableWindow(items);
  if (pages.error instanceof ApiError && pages.error.status === 403) return <ForbiddenPage />;

  const filtered = filters.q !== "" || filters.status !== "";

  return (
    <section aria-labelledby="page-title">
      <div className="page-header">
        <h1 id="page-title">Packages</h1>
        <Link to="/packages/new">Create package</Link>
      </div>
      {deleted ? (
        <p className="banner ok" role="status">
          Deleted <span dir="auto">{deleted}</span>.
        </p>
      ) : null}
      <form className="toolbar" onSubmit={apply} aria-label="Filter packages">
        <div className="field">
          <label htmlFor="filter-q">Search title or slug</label>
          <input
            id="filter-q"
            type="search"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            aria-invalid={tooLong || undefined}
            aria-describedby={tooLong ? "filter-q-error" : undefined}
          />
          {tooLong ? (
            <span id="filter-q-error" className="field-error">
              At most 100 characters.
            </span>
          ) : null}
        </div>
        <div className="field">
          <label htmlFor="filter-status">Status</label>
          <select
            id="filter-status"
            value={status}
            onChange={(e) => setStatus(e.target.value as PackageStatus | "")}
          >
            <option value="">Any status</option>
            {STATUSES.map((s) => (
              <option key={s} value={s}>
                {STATUS_LABEL[s]}
              </option>
            ))}
          </select>
        </div>
        <button type="submit">Apply</button>
        {filtered ? (
          <button
            type="button"
            onClick={() => {
              setQ("");
              setStatus("");
              setParams(new URLSearchParams());
            }}
          >
            Clear filters
          </button>
        ) : null}
      </form>

      {pages.isPending ? (
        <Loading label="Loading packages…" />
      ) : pages.isError && !pages.data ? (
        <ErrorView error={pages.error} onRetry={() => void pages.refetch()} />
      ) : items.length === 0 ? (
        filtered ? (
          <p role="status">No packages match these filters.</p>
        ) : (
          <div role="status">
            <p>No packages yet.</p>
            <p>
              <Link to="/packages/new">Create the first package</Link>
            </p>
          </div>
        )
      ) : (
        <>
          <div className="table-wrap" ref={win.scrollRef}>
            <table>
              <caption className="visually-hidden">
                Packages{filtered ? " (filtered)" : ""}, {items.length} shown
              </caption>
              <thead>
                <tr>
                  <th scope="col">Title</th>
                  <th scope="col">Slug</th>
                  <th scope="col">Status</th>
                  <th scope="col">Platforms</th>
                  <th scope="col">Updated</th>
                </tr>
              </thead>
              <tbody>
                <SpacerRow height={win.before} columns={5} />
                {win.rows.map(({ item: p, index }) => (
                  <tr key={p.id} ref={win.measure} data-index={index}>
                    <td>
                      <Link to={`/packages/${p.id}`} dir="auto">
                        {p.title}
                      </Link>
                    </td>
                    <td className="mono">{p.slug}</td>
                    <td>{STATUS_LABEL[p.status]}</td>
                    <td>{p.platforms.length > 0 ? p.platforms.join(", ") : "—"}</td>
                    <td>
                      <time dateTime={p.updated_at}>{new Date(p.updated_at).toLocaleString()}</time>
                    </td>
                  </tr>
                ))}
                <SpacerRow height={win.after} columns={5} />
              </tbody>
            </table>
          </div>
          <div className="row" style={{ marginTop: 8 }}>
            <span className="muted" role="status">
              {items.length} shown{pages.hasNextPage ? ", more available" : ""}
            </span>
            {pages.hasNextPage ? (
              <button
                type="button"
                disabled={pages.isFetchingNextPage}
                onClick={() => void pages.fetchNextPage()}
              >
                {pages.isFetchingNextPage ? "Loading…" : "Load more"}
              </button>
            ) : null}
          </div>
          {pages.isFetchNextPageError ? (
            <ErrorView error={pages.error} onRetry={() => void pages.fetchNextPage()} />
          ) : null}
        </>
      )}
    </section>
  );
}
