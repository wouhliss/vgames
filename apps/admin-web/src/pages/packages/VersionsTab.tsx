// Versions (A3-T16): every version with its state, sequence, label, platform, size, creator, the
// current-release badge and verification progress (polled every 2 s while one verifies). Actions:
// continue an upload, abort an unpublished version, publish a ready one, yank a published one (with
// a reason). Nothing is optimistic: the table shows what the server confirmed.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Link } from "react-router";
import { ApiError } from "../../api/errors";
import type { Version } from "../../api/schemas";
import {
  abortVersion,
  listVersions,
  publishVersion,
  versionKeys,
  yankVersion,
} from "../../api/versions";
import { ErrorView, Loading } from "../../app/ErrorView";
import { Modal } from "../../components/Modal";
import { usePackageContext } from "./PackageLayout";

export const VERSION_POLL_MS = 2000;

export const STATE_LABEL: Record<Version["state"], string> = {
  uploading: "Uploading",
  verifying: "Verifying",
  ready: "Ready to publish",
  published: "Published",
  failed: "Failed",
  yanked: "Withdrawn",
  aborted: "Aborted",
};

export function formatSize(bytes: number | undefined): string {
  if (bytes === undefined) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = bytes;
  let u = 0;
  while (v >= 1000 && u < units.length - 1) {
    v /= 1000;
    u += 1;
  }
  return `${v.toLocaleString("en", { maximumFractionDigits: u === 0 ? 0 : 1 })} ${units[u]}`;
}

type Pending =
  | { kind: "publish"; version: Version }
  | { kind: "yank"; version: Version }
  | { kind: "abort"; version: Version }
  | null;

export function VersionsTab() {
  const { response } = usePackageContext();
  const pkg = response.data;
  const client = useQueryClient();
  const versions = useQuery({
    queryKey: versionKeys.list(pkg.id),
    queryFn: async ({ signal }) => (await listVersions(pkg.id, null, signal)).data,
    refetchInterval: (q) =>
      q.state.data?.items.some((v) => v.state === "verifying") ? VERSION_POLL_MS : false,
  });
  const [pending, setPending] = useState<Pending>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);

  const act = async (run: () => Promise<unknown>, done: string) => {
    setFailure(null);
    try {
      await run();
      setMessage(done);
    } catch (e) {
      setFailure(e);
    } finally {
      setPending(null);
      await client.invalidateQueries({ queryKey: versionKeys.list(pkg.id) });
    }
  };

  return (
    <div>
      <div className="page-header">
        <h2>Versions</h2>
        <Link to={`/packages/${pkg.id}/versions/new`}>Upload a new version</Link>
      </div>
      {message ? (
        <p role="status" className="banner ok">
          {message}
        </p>
      ) : null}
      {failure ? <ErrorView error={failure} /> : null}
      {versions.isPending ? (
        <Loading label="Loading versions…" />
      ) : versions.isError && !versions.data ? (
        <ErrorView error={versions.error} onRetry={() => void versions.refetch()} />
      ) : versions.data.items.length === 0 ? (
        <p role="status">No versions yet. Upload the first one.</p>
      ) : (
        <div className="table-wrap">
          <table>
            <caption className="visually-hidden">Versions, newest first</caption>
            <thead>
              <tr>
                <th scope="col">#</th>
                <th scope="col">Version</th>
                <th scope="col">Platform</th>
                <th scope="col">State</th>
                <th scope="col">Size</th>
                <th scope="col">Created by</th>
                <th scope="col">Actions</th>
              </tr>
            </thead>
            <tbody>
              {versions.data.items.map((v) => (
                <tr key={v.id}>
                  <td>{v.sequence}</td>
                  <th scope="row" dir="auto">
                    {v.version_label}
                    {v.is_current_release ? (
                      <>
                        {" "}
                        <span className="badge">Current release</span>
                      </>
                    ) : null}
                  </th>
                  <td className="mono">{v.platform}</td>
                  <td>
                    {STATE_LABEL[v.state]}
                    {v.state === "verifying" ? (
                      <>
                        {" "}
                        <progress
                          max={1}
                          value={v.verify_progress ?? 0}
                          aria-label={`Verifying ${v.version_label}`}
                        />{" "}
                        {Math.round((v.verify_progress ?? 0) * 100)}%
                      </>
                    ) : null}
                    {v.state === "failed" && v.failure_reason ? (
                      <div className="field-error">{v.failure_reason}</div>
                    ) : null}
                  </td>
                  <td>
                    {formatSize(v.total_size)}
                    {v.file_count !== undefined ? (
                      <div className="muted">{v.file_count.toLocaleString("en")} files</div>
                    ) : null}
                  </td>
                  <td>{v.created_by.display_name ?? v.created_by.username}</td>
                  <td>
                    <div className="row">
                      {v.state === "uploading" ? (
                        <Link to={`/packages/${pkg.id}/versions/${v.id}/upload`}>
                          Continue upload
                        </Link>
                      ) : null}
                      {v.state === "ready" ? (
                        <button
                          type="button"
                          className="primary"
                          onClick={() => setPending({ kind: "publish", version: v })}
                        >
                          Publish…
                        </button>
                      ) : null}
                      {v.state === "published" ? (
                        <button
                          type="button"
                          className="danger"
                          onClick={() => setPending({ kind: "yank", version: v })}
                        >
                          Yank…
                        </button>
                      ) : null}
                      {["uploading", "verifying", "ready", "failed"].includes(v.state) ? (
                        <button
                          type="button"
                          onClick={() => setPending({ kind: "abort", version: v })}
                        >
                          Abort…
                        </button>
                      ) : null}
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {pending?.kind === "publish" ? (
        <Modal title={`Publish ${pending.version.version_label}?`} onClose={() => setPending(null)}>
          <p>
            It becomes the current release for {pending.version.platform}. Players get it as an
            update right away.
          </p>
          <div className="actions">
            <button type="button" data-autofocus="" onClick={() => setPending(null)}>
              Cancel
            </button>
            <button
              type="button"
              className="primary"
              onClick={() =>
                void act(
                  () => publishVersion(pending.version.id),
                  `Published ${pending.version.version_label}.`,
                )
              }
            >
              Publish
            </button>
          </div>
        </Modal>
      ) : null}
      {pending?.kind === "abort" ? (
        <Modal
          title={`Abort ${pending.version.version_label}?`}
          role="alertdialog"
          onClose={() => setPending(null)}
        >
          <p>The upload stops and its files are deleted from storage. This can't be undone.</p>
          <div className="actions">
            <button type="button" data-autofocus="" onClick={() => setPending(null)}>
              Keep it
            </button>
            <button
              type="button"
              className="danger"
              onClick={() =>
                void act(
                  () => abortVersion(pending.version.id),
                  `Aborted ${pending.version.version_label}.`,
                )
              }
            >
              Abort version
            </button>
          </div>
        </Modal>
      ) : null}
      {pending?.kind === "yank" ? (
        <YankDialog
          version={pending.version}
          onCancel={() => setPending(null)}
          onYank={(reason) =>
            act(
              () => yankVersion(pending.version.id, reason),
              `Withdrew ${pending.version.version_label}.`,
            )
          }
        />
      ) : null}
    </div>
  );
}

function YankDialog({
  version,
  onCancel,
  onYank,
}: {
  version: Version;
  onCancel: () => void;
  onYank: (reason: string) => Promise<void>;
}) {
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const n = [...reason.trim()].length;
  return (
    <Modal
      title={`Yank ${version.version_label}?`}
      role="alertdialog"
      busy={busy}
      onClose={onCancel}
    >
      <p>
        Players on this version are offered the current release instead, even if it is older. The
        version stays on the server but can't be installed anymore.
      </p>
      <form
        onSubmit={async (e) => {
          e.preventDefault();
          if (n < 3 || n > 500) {
            setError("Give a reason of 3 to 500 characters. Players and admins see it.");
            return;
          }
          setBusy(true);
          try {
            await onYank(reason.trim());
          } catch (err) {
            if (err instanceof ApiError) setError(err.message);
          } finally {
            setBusy(false);
          }
        }}
        noValidate
      >
        <div className="field">
          <label htmlFor="yank-reason">Reason (required)</label>
          <textarea
            id="yank-reason"
            rows={3}
            value={reason}
            aria-invalid={error ? true : undefined}
            aria-describedby={error ? "yank-reason-error" : "yank-reason-count"}
            onChange={(e) => {
              setReason(e.target.value);
              setError(null);
            }}
          />
          <span id="yank-reason-count" className={n > 500 ? "field-error" : "muted"}>
            {n} / 500
          </span>
          {error ? (
            <span id="yank-reason-error" className="field-error">
              {error}
            </span>
          ) : null}
        </div>
        <div className="actions">
          <button type="button" onClick={onCancel} disabled={busy}>
            Cancel
          </button>
          <button type="submit" className="danger" disabled={busy}>
            {busy ? "Yanking…" : "Yank version"}
          </button>
        </div>
      </form>
    </Modal>
  );
}
