// Metadata review (A3-T15): the lookup job (polled every 2 s while queued or running, stopped at a
// terminal state), the candidates (source, title, year, score), a side-by-side comparison with the
// current values and a checkbox per field, and "overwrite fields I edited" behind a warning. Apply
// uses If-Match; a 412 asks to reload and review.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { ApiError } from "../../api/errors";
import {
  applyMetadata,
  listCandidates,
  metadataKeys,
  packageKeys,
  refreshMetadata,
} from "../../api/packages";
import type { AdminPackage, Job, MetadataCandidate } from "../../api/schemas";
import { ErrorView, Loading } from "../../app/ErrorView";
import {
  APPLY_FIELDS,
  type ApplyField,
  candidateKey,
  candidateValue,
  currentValue,
  defaultSelection,
  editedByAdmin,
} from "./metadataModel";
import { usePackageContext } from "./PackageLayout";

export const POLL_MS = 2000;
const ACTIVE = new Set<Job["state"]>(["queued", "running"]);

const JOB_TEXT: Record<Job["state"], string> = {
  queued: "Waiting to look up metadata…",
  running: "Looking up IGDB and Steam…",
  succeeded: "Lookup finished.",
  failed: "The lookup failed and will be retried automatically.",
  dead: "The lookup failed and gave up.",
};

export function MetadataTab() {
  const { response } = usePackageContext();
  const pkg = response.data;
  const client = useQueryClient();
  const candidates = useQuery({
    queryKey: metadataKeys.candidates(pkg.id),
    queryFn: async ({ signal }) => (await listCandidates(pkg.id, signal)).data,
    refetchInterval: (query) => {
      const state = query.state.data?.job?.state;
      return state && ACTIVE.has(state) ? POLL_MS : false;
    },
  });
  const [refreshing, setRefreshing] = useState(false);
  const [refreshError, setRefreshError] = useState<unknown>(null);
  const [chosen, setChosen] = useState<string | null>(null);
  const [applied, setApplied] = useState<string | null>(null);

  const refresh = async () => {
    setRefreshing(true);
    setRefreshError(null);
    try {
      const { data: job } = await refreshMetadata(pkg.id);
      client.setQueryData(metadataKeys.candidates(pkg.id), (old: typeof candidates.data) =>
        old ? { ...old, job } : { items: [], job },
      );
      setChosen(null);
    } catch (error) {
      setRefreshError(error);
    } finally {
      setRefreshing(false);
    }
  };

  if (candidates.isPending) return <Loading label="Loading candidates…" />;
  if (candidates.isError && !candidates.data)
    return <ErrorView error={candidates.error} onRetry={() => void candidates.refetch()} />;

  const job = candidates.data.job;
  const busy = job ? ACTIVE.has(job.state) : false;
  const items = candidates.data.items;
  const selected = items.find((c) => candidateKey(c) === chosen) ?? null;

  return (
    <div>
      <h2>Lookup</h2>
      <div role="status" className="row">
        {job ? (
          <span>
            {JOB_TEXT[job.state]}{" "}
            {job.state === "failed" || job.state === "dead"
              ? `(attempt ${job.attempts} of ${job.max_attempts})`
              : ""}
          </span>
        ) : (
          <span className="muted">No lookup has run for this package yet.</span>
        )}
      </div>
      {job?.last_error && (job.state === "failed" || job.state === "dead") ? (
        <p className="banner error">
          <span className="mono">{job.last_error}</span>
        </p>
      ) : null}
      <p>
        <button type="button" onClick={() => void refresh()} disabled={refreshing || busy}>
          {job?.state === "failed" || job?.state === "dead"
            ? "Try again"
            : refreshing
              ? "Starting…"
              : "Look up again"}
        </button>
      </p>
      {refreshError ? <ErrorView error={refreshError} /> : null}
      {candidates.isRefetchError ? (
        <ErrorView error={candidates.error} onRetry={() => void candidates.refetch()} />
      ) : null}

      <h2>Candidates</h2>
      {items.length === 0 ? (
        <p role="status">
          {busy
            ? "Candidates appear here when the lookup finishes."
            : "No candidates found. Check the Steam and IGDB ids on the Details tab, then look up again."}
        </p>
      ) : (
        <div className="table-wrap">
          <table>
            <caption className="visually-hidden">Metadata candidates, best first</caption>
            <thead>
              <tr>
                <th scope="col">Compare</th>
                <th scope="col">Source</th>
                <th scope="col">Title</th>
                <th scope="col">Year</th>
                <th scope="col">Score</th>
              </tr>
            </thead>
            <tbody>
              {items.map((c) => (
                <tr key={candidateKey(c)}>
                  <td>
                    <input
                      type="radio"
                      name="candidate"
                      aria-label={`Compare ${c.title} (${c.source.toUpperCase()} ${c.external_id})`}
                      checked={chosen === candidateKey(c)}
                      onChange={() => {
                        setChosen(candidateKey(c));
                        setApplied(null);
                      }}
                    />
                  </td>
                  <td>{c.source === "igdb" ? "IGDB" : "Steam"}</td>
                  <td dir="auto">{c.title}</td>
                  <td>{c.release_year ?? "—"}</td>
                  <td>{Math.round(c.score * 100)}%</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {selected ? (
        <Compare
          key={candidateKey(selected)}
          pkg={pkg}
          etag={response.etag ?? ""}
          candidate={selected}
          onApplied={(message) => {
            setApplied(message);
            setChosen(null);
          }}
        />
      ) : null}
      {applied ? (
        <p role="status" className="banner ok">
          {applied}
        </p>
      ) : null}
    </div>
  );
}

function Compare({
  pkg,
  etag,
  candidate,
  onApplied,
}: {
  pkg: AdminPackage;
  etag: string;
  candidate: MetadataCandidate;
  onApplied: (message: string) => void;
}) {
  const client = useQueryClient();
  const [picked, setPicked] = useState(() => defaultSelection(pkg, candidate));
  const [overwrite, setOverwrite] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const toggle = (name: ApplyField, on: boolean) =>
    setPicked((prev) => {
      const next = new Set(prev);
      if (on) next.add(name);
      else next.delete(name);
      return next;
    });

  // Admin-edited fields only go when "overwrite" is on.
  const fields = [...picked].filter((f) => overwrite || !editedByAdmin(pkg, f));
  const skipped = [...picked].filter((f) => !overwrite && editedByAdmin(pkg, f));

  const apply = async () => {
    if (fields.length === 0 || busy) return;
    setBusy(true);
    setError(null);
    try {
      const result = await applyMetadata(pkg.id, etag, {
        source: candidate.source,
        external_id: candidate.external_id,
        fields,
        overwrite_admin_fields: overwrite,
      });
      client.setQueryData(packageKeys.one(pkg.id), result);
      void client.invalidateQueries({ queryKey: ["admin-packages"] });
      onApplied(
        `Applied ${fields.length} field${fields.length === 1 ? "" : "s"}. Images are fetched in the background.`,
      );
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };

  const conflict = error instanceof ApiError && error.status === 412;

  return (
    <section aria-labelledby="compare-title">
      <h2 id="compare-title">
        Compare with {candidate.source === "igdb" ? "IGDB" : "Steam"}:{" "}
        <span dir="auto">{candidate.title}</span>
      </h2>
      <div className="table-wrap">
        <table className="diff">
          <caption className="visually-hidden">Current values and the candidate's values</caption>
          <thead>
            <tr>
              <th scope="col">Apply</th>
              <th scope="col">Field</th>
              <th scope="col">Current</th>
              <th scope="col">Candidate</th>
            </tr>
          </thead>
          <tbody>
            {APPLY_FIELDS.map(({ name, label }) => {
              const theirs = candidateValue(candidate, name);
              const edited = editedByAdmin(pkg, name);
              return (
                <tr key={name}>
                  <td>
                    <input
                      type="checkbox"
                      aria-label={`Apply ${label}`}
                      checked={picked.has(name)}
                      disabled={theirs === ""}
                      onChange={(e) => toggle(name, e.target.checked)}
                    />
                  </td>
                  <th scope="row">
                    {label}
                    {edited ? (
                      <>
                        {" "}
                        <span className="badge">Edited by an admin</span>
                      </>
                    ) : null}
                  </th>
                  <td dir="auto">{currentValue(pkg, name) || "(empty)"}</td>
                  <td dir="auto">{theirs || "(none)"}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      <div className="field">
        <label>
          <input
            type="checkbox"
            checked={overwrite}
            onChange={(e) => setOverwrite(e.target.checked)}
          />{" "}
          Overwrite fields an admin edited
        </label>
        {overwrite ? (
          <p className="banner warn">
            Fields an admin typed by hand will be replaced by the candidate's values. This can't be
            undone here; the old text is only in the audit log.
          </p>
        ) : skipped.length > 0 ? (
          <p className="muted">
            {skipped.length} ticked field{skipped.length === 1 ? " was" : "s were"} edited by an
            admin and will be kept. Turn on "Overwrite fields an admin edited" to replace them.
          </p>
        ) : null}
      </div>
      {conflict ? (
        <div className="banner warn" role="alert">
          <strong>Changed by someone else</strong>
          <p>
            The package changed since you loaded it. Reload it, check the comparison again, then
            apply.
          </p>
          <button
            type="button"
            onClick={async () => {
              await client.invalidateQueries({ queryKey: packageKeys.one(pkg.id) });
              setError(null);
            }}
          >
            Reload package
          </button>
        </div>
      ) : error ? (
        <ErrorView error={error} />
      ) : null}
      <button
        type="button"
        className="primary"
        disabled={fields.length === 0 || busy}
        onClick={() => void apply()}
      >
        {busy ? "Applying…" : `Apply ${fields.length} field${fields.length === 1 ? "" : "s"}`}
      </button>
    </section>
  );
}
