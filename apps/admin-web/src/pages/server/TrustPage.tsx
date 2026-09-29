// Trust (A3-T17): the current trust bundle's publisher keys with their holder and validity, a warning
// when one expires within 60 days, revoked keys marked. Owners upload a new root-signed bundle (the
// bundle JSON and its signature file, both made with the offline CLI).
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { ApiError } from "../../api/errors";
import { listPublisherKeys, serverKeys, uploadBundle } from "../../api/server";
import { ErrorView, ForbiddenPage, Loading } from "../../app/ErrorView";
import { toBase64 } from "../packages/compatModel";
import { useRole, when } from "./shared";

const DAY = 86_400_000;
export const WARN_DAYS = 60;

export const daysLeft = (iso: string, now = Date.now()) =>
  Math.floor((Date.parse(iso) - now) / DAY);

const BUNDLE_ERRORS: Record<string, string> = {
  bad_signature:
    "The signature isn't from this server's root key. Sign the bundle with the root key of this server.",
  wrong_server: "This bundle was made for another server.",
  invalid_bundle: "The file isn't a valid trust bundle.",
  stale_version:
    "The server already has this bundle version or a newer one. Make a bundle with a higher version.",
  unknown_holder: "A key in the bundle names a holder who isn't a user of this server.",
};

async function asBase64(file: File): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  const text = new TextDecoder().decode(bytes).trim();
  // A signature file may already be base64 text; anything else is encoded here.
  return /^[A-Za-z0-9+/]+={0,2}$/.test(text) && file.name.endsWith(".sig") ? text : toBase64(bytes);
}

export function TrustPage() {
  const role = useRole();
  const client = useQueryClient();
  const keys = useQuery({
    queryKey: serverKeys.trust,
    queryFn: async ({ signal }) => (await listPublisherKeys(signal)).data,
  });
  const [bundle, setBundle] = useState<File | null>(null);
  const [signature, setSignature] = useState<File | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  if (keys.isPending) return <Loading label="Loading trusted keys…" />;
  if (keys.isError) {
    if (keys.error instanceof ApiError && keys.error.status === 403) return <ForbiddenPage />;
    return <ErrorView error={keys.error} onRetry={() => void keys.refetch()} />;
  }

  const upload = async (e: FormEvent) => {
    e.preventDefault();
    setProblem(null);
    setFailure(null);
    setMessage(null);
    if (!bundle || !signature) {
      setProblem("Choose both the bundle file and its signature file.");
      return;
    }
    setBusy(true);
    try {
      const { data } = await uploadBundle({
        bundle: await asBase64(bundle),
        signature: await asBase64(signature),
      });
      setMessage(`Trust bundle version ${data.version} is active.`);
      await client.invalidateQueries({ queryKey: serverKeys.trust });
    } catch (err) {
      const text = err instanceof ApiError ? BUNDLE_ERRORS[err.code ?? ""] : undefined;
      if (text) setProblem(text);
      else setFailure(err);
    } finally {
      setBusy(false);
    }
  };

  const bundleDays = keys.data.bundle_expires_at ? daysLeft(keys.data.bundle_expires_at) : null;

  return (
    <section aria-labelledby="page-title">
      <h1 id="page-title">Trust</h1>
      <p>
        Trust bundle version {keys.data.bundle_version}
        {keys.data.bundle_expires_at ? `, valid until ${when(keys.data.bundle_expires_at)}` : ""}.
      </p>
      {bundleDays !== null && bundleDays < WARN_DAYS ? (
        <p className="banner warn" role="status">
          The trust bundle expires in {Math.max(0, bundleDays)} days. Launchers stop installing new
          versions when it expires: sign and upload a new one before then.
        </p>
      ) : null}
      {keys.data.items.length === 0 ? (
        <p role="status">
          No publisher keys yet. Upload a trust bundle that lists them to publish packages.
        </p>
      ) : (
        <div className="table-wrap">
          <table>
            <caption className="visually-hidden">Publisher keys</caption>
            <thead>
              <tr>
                <th scope="col">Key</th>
                <th scope="col">Holder</th>
                <th scope="col">Valid</th>
                <th scope="col">Status</th>
              </tr>
            </thead>
            <tbody>
              {keys.data.items.map((k) => {
                const left = daysLeft(k.not_after);
                return (
                  <tr key={k.key_id}>
                    <th scope="row">
                      {k.label} <div className="mono muted">{k.key_id}</div>
                    </th>
                    <td>{k.holder.display_name ?? k.holder.username}</td>
                    <td>
                      {when(k.not_before)} – {when(k.not_after)}
                    </td>
                    <td>
                      {k.revoked_at ? (
                        <>
                          <span className="badge">Revoked</span> {when(k.revoked_at)}
                          {k.revocation_reason ? (
                            <div className="muted">{k.revocation_reason}</div>
                          ) : null}
                        </>
                      ) : left < 0 ? (
                        <span className="badge">Expired</span>
                      ) : left < WARN_DAYS ? (
                        <span className="field-error">Expires in {left} days</span>
                      ) : (
                        "Valid"
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      {role === "owner" ? (
        <form onSubmit={(e) => void upload(e)} aria-label="Upload a trust bundle" noValidate>
          <h2>Upload a new trust bundle</h2>
          <p className="muted">
            Made and signed with the root key on the offline machine (vgames CLI).
          </p>
          <div className="toolbar">
            <div className="field">
              <label htmlFor="bundle-file">Bundle (.json)</label>
              <input
                id="bundle-file"
                type="file"
                accept=".json,application/json"
                onChange={(e) => setBundle(e.target.files?.[0] ?? null)}
              />
            </div>
            <div className="field">
              <label htmlFor="bundle-sig">Signature (.sig)</label>
              <input
                id="bundle-sig"
                type="file"
                accept=".sig"
                onChange={(e) => setSignature(e.target.files?.[0] ?? null)}
              />
            </div>
            <button type="submit" className="primary" aria-disabled={busy || undefined}>
              {busy ? "Uploading…" : "Upload bundle"}
            </button>
          </div>
          {problem ? (
            <p className="banner error" role="alert">
              {problem}
            </p>
          ) : null}
          {failure ? <ErrorView error={failure} /> : null}
          {message ? (
            <p className="banner ok" role="status">
              {message}
            </p>
          ) : null}
        </form>
      ) : (
        <p className="muted">Only an owner can upload a new trust bundle.</p>
      )}
    </section>
  );
}
