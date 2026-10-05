// Audit log (A3-T17): newest first, filters (actor, action, target, date range) kept in the URL,
// cursor paging, and each entry's details as JSON text (never markup).
import { useInfiniteQuery } from "@tanstack/react-query";
import { type FormEvent, useEffect, useState } from "react";
import { ApiError } from "../../api/errors";
import { type AuditFilters, listAudit, serverKeys } from "../../api/server";
import { ErrorView, ForbiddenPage, Loading } from "../../app/ErrorView";
import { SpacerRow, useTableWindow } from "../../components/tableWindow";
import { LoadMore, useUrlFilters, when } from "./shared";

const KEYS = ["actor", "action", "targetType", "targetId", "since", "until"] as const;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** `datetime-local` value (local time) ↔ ISO timestamp. */
const toIso = (local: string) => (local ? new Date(local).toISOString() : "");
const toLocal = (iso: string) => {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
};

export function AuditPage() {
  const { values, apply } = useUrlFilters(KEYS);
  const [form, setForm] = useState<AuditFilters>(values);
  const [error, setError] = useState<string | null>(null);
  // Back/forward and links change the URL: the form follows. (`key` stands for `values`, which is a
  // new object on every render.)
  const key = JSON.stringify(values);
  useEffect(() => setForm(JSON.parse(key) as AuditFilters), [key]);
  const pages = useInfiniteQuery({
    queryKey: serverKeys.audit(values),
    queryFn: async ({ pageParam, signal }) => (await listAudit(values, pageParam, signal)).data,
    initialPageParam: null as string | null,
    getNextPageParam: (last) => last.next_cursor ?? null,
  });
  const entries = pages.data?.pages.flatMap((p) => p.items) ?? [];
  const win = useTableWindow(entries);
  if (pages.error instanceof ApiError && pages.error.status === 403) return <ForbiddenPage />;
  const set = (k: keyof AuditFilters, v: string) => setForm((f) => ({ ...f, [k]: v }));

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (form.actor.trim() && !UUID.test(form.actor.trim())) {
      setError("The actor is a user id (a UUID, as shown on the Users page).");
      return;
    }
    if (form.since && form.until && form.since > form.until) {
      setError("The start of the date range is after its end.");
      return;
    }
    setError(null);
    apply(form);
  };

  return (
    <section aria-labelledby="page-title">
      <h1 id="page-title">Audit log</h1>
      <form className="toolbar" aria-label="Filter the audit log" onSubmit={submit} noValidate>
        <div className="field">
          <label htmlFor="audit-actor">Actor (user id)</label>
          <input
            id="audit-actor"
            className="mono"
            value={form.actor}
            onChange={(e) => set("actor", e.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor="audit-action">Action</label>
          <input
            id="audit-action"
            className="mono"
            maxLength={64}
            value={form.action}
            onChange={(e) => set("action", e.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor="audit-type">Target type</label>
          <input
            id="audit-type"
            className="mono"
            maxLength={64}
            value={form.targetType}
            onChange={(e) => set("targetType", e.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor="audit-target">Target id</label>
          <input
            id="audit-target"
            className="mono"
            maxLength={128}
            value={form.targetId}
            onChange={(e) => set("targetId", e.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor="audit-since">From</label>
          <input
            id="audit-since"
            type="datetime-local"
            value={toLocal(form.since)}
            onChange={(e) => set("since", toIso(e.target.value))}
          />
        </div>
        <div className="field">
          <label htmlFor="audit-until">To</label>
          <input
            id="audit-until"
            type="datetime-local"
            value={toLocal(form.until)}
            onChange={(e) => set("until", toIso(e.target.value))}
          />
        </div>
        <button type="submit">Apply</button>
      </form>
      {error ? (
        <p className="banner error" role="alert">
          {error}
        </p>
      ) : null}
      {pages.isPending ? (
        <Loading label="Loading the audit log…" />
      ) : pages.isError && !pages.data ? (
        <ErrorView error={pages.error} onRetry={() => void pages.refetch()} />
      ) : entries.length === 0 ? (
        <p role="status">No entries match.</p>
      ) : (
        <>
          <div className="table-wrap" ref={win.scrollRef}>
            <table>
              <caption className="visually-hidden">Audit entries, newest first</caption>
              <thead>
                <tr>
                  <th scope="col">When</th>
                  <th scope="col">Actor</th>
                  <th scope="col">Action</th>
                  <th scope="col">Target</th>
                  <th scope="col">Details</th>
                </tr>
              </thead>
              <tbody>
                <SpacerRow height={win.before} columns={5} />
                {win.rows.map(({ item: e, index }) => (
                  <tr key={e.id} ref={win.measure} data-index={index}>
                    <td>{when(e.created_at)}</td>
                    <td>{e.actor ? (e.actor.display_name ?? e.actor.username) : "system"}</td>
                    <th scope="row" className="mono">
                      {e.action}
                    </th>
                    <td className="mono">
                      {e.target_type ?? ""} {e.target_id ?? ""}
                    </td>
                    <td>
                      {e.details ? (
                        <details>
                          <summary>Details</summary>
                          <pre className="mono">{JSON.stringify(e.details, null, 2)}</pre>
                        </details>
                      ) : null}
                    </td>
                  </tr>
                ))}
                <SpacerRow height={win.after} columns={5} />
              </tbody>
            </table>
          </div>
          <LoadMore query={pages} count={entries.length} />
        </>
      )}
    </section>
  );
}
