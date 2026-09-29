// Jobs (A3-T17): background jobs by state and kind, their last error as text, and retry for failed
// or dead ones.
import { useInfiniteQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useEffect, useState } from "react";
import { ApiError } from "../../api/errors";
import { JobStateSchema } from "../../api/schemas";
import { listJobs, retryJob, serverKeys } from "../../api/server";
import { ErrorView, ForbiddenPage, Loading } from "../../app/ErrorView";
import { SpacerRow, useTableWindow } from "../../components/tableWindow";
import { LoadMore, useUrlFilters, when } from "./shared";

export function JobsPage() {
  const client = useQueryClient();
  const { values, apply } = useUrlFilters(["state", "kind"] as const);
  const [state, setState] = useState(values.state);
  const [kind, setKind] = useState(values.kind);
  useEffect(() => {
    setState(values.state);
    setKind(values.kind);
  }, [values.state, values.kind]);
  const pages = useInfiniteQuery({
    queryKey: serverKeys.jobs(values),
    queryFn: async ({ pageParam, signal }) => (await listJobs(values, pageParam, signal)).data,
    initialPageParam: null as string | null,
    getNextPageParam: (last) => last.next_cursor ?? null,
  });
  const [message, setMessage] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);

  const jobs = pages.data?.pages.flatMap((p) => p.items) ?? [];
  const win = useTableWindow(jobs);
  if (pages.error instanceof ApiError && pages.error.status === 403) return <ForbiddenPage />;

  return (
    <section aria-labelledby="page-title">
      <h1 id="page-title">Jobs</h1>
      <form
        className="toolbar"
        aria-label="Filter jobs"
        onSubmit={(e: FormEvent) => {
          e.preventDefault();
          apply({ state, kind });
        }}
      >
        <div className="field">
          <label htmlFor="jobs-state">State</label>
          <select id="jobs-state" value={state} onChange={(e) => setState(e.target.value)}>
            <option value="">Any state</option>
            {JobStateSchema.options.map((s) => (
              <option key={s} value={s}>
                {s}
              </option>
            ))}
          </select>
        </div>
        <div className="field">
          <label htmlFor="jobs-kind">Kind</label>
          <input
            id="jobs-kind"
            className="mono"
            maxLength={64}
            value={kind}
            onChange={(e) => setKind(e.target.value)}
          />
        </div>
        <button type="submit">Apply</button>
      </form>
      {message ? (
        <p className="banner ok" role="status">
          {message}
        </p>
      ) : null}
      {failure ? (
        failure instanceof ApiError && failure.code === "job_already_queued" ? (
          <p className="banner warn" role="alert">
            That job is already queued or running.
          </p>
        ) : (
          <ErrorView error={failure} />
        )
      ) : null}
      {pages.isPending ? (
        <Loading label="Loading jobs…" />
      ) : pages.isError && !pages.data ? (
        <ErrorView error={pages.error} onRetry={() => void pages.refetch()} />
      ) : jobs.length === 0 ? (
        <p role="status">No jobs match.</p>
      ) : (
        <>
          <div className="table-wrap" ref={win.scrollRef}>
            <table>
              <caption className="visually-hidden">Jobs</caption>
              <thead>
                <tr>
                  <th scope="col">Kind</th>
                  <th scope="col">State</th>
                  <th scope="col">Attempts</th>
                  <th scope="col">Last error</th>
                  <th scope="col">Created</th>
                  <th scope="col">Actions</th>
                </tr>
              </thead>
              <tbody>
                <SpacerRow height={win.before} columns={6} />
                {win.rows.map(({ item: j, index }) => (
                  <tr key={j.id} ref={win.measure} data-index={index}>
                    <th scope="row" className="mono">
                      {j.kind}
                    </th>
                    <td>{j.state}</td>
                    <td>
                      {j.attempts} / {j.max_attempts}
                    </td>
                    <td className="mono">{j.last_error ?? ""}</td>
                    <td>{when(j.created_at)}</td>
                    <td>
                      {j.state === "failed" || j.state === "dead" ? (
                        <button
                          type="button"
                          onClick={async () => {
                            setMessage(null);
                            setFailure(null);
                            try {
                              await retryJob(j.id);
                              setMessage(`${j.kind} is queued again.`);
                            } catch (e) {
                              setFailure(e);
                            }
                            await client.invalidateQueries({ queryKey: ["admin-jobs"] });
                          }}
                        >
                          Retry {j.kind}
                        </button>
                      ) : null}
                    </td>
                  </tr>
                ))}
                <SpacerRow height={win.after} columns={6} />
              </tbody>
            </table>
          </div>
          <LoadMore query={pages} count={jobs.length} />
        </>
      )}
    </section>
  );
}
