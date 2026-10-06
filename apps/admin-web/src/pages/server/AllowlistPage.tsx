// Allowlist (A3-T17): who may sign in while registration is "allowlist". Add by Discord id (5–25
// digits, checked before sending) with an optional note; remove after confirmation.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { z } from "zod";
import { ApiError } from "../../api/errors";
import { addAllowlist, listAllowlist, removeAllowlist, serverKeys } from "../../api/server";
import { RESTORED_MESSAGE, useDraft } from "../../app/drafts";
import { ErrorView, ForbiddenPage, Loading } from "../../app/ErrorView";
import { Modal } from "../../components/Modal";
import { when } from "./shared";

const AddDraft = z.object({ id: z.string(), note: z.string() });
type AddDraft = z.infer<typeof AddDraft>;

export function AllowlistPage() {
  const client = useQueryClient();
  const list = useQuery({
    queryKey: serverKeys.allowlist,
    queryFn: async ({ signal }) => (await listAllowlist(signal)).data.items,
  });
  const restored = useDraft("allowlist-add", AddDraft, (): AddDraft | null =>
    id || note ? { id, note } : null,
  );
  const [id, setId] = useState(restored?.id ?? "");
  const [note, setNote] = useState(restored?.note ?? "");
  const [idError, setIdError] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);
  const [message, setMessage] = useState<string | null>(restored ? RESTORED_MESSAGE : null);
  const [busy, setBusy] = useState(false);
  const [removing, setRemoving] = useState<string | null>(null);

  const add = async (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    const value = id.trim();
    setMessage(null);
    setFailure(null);
    if (!/^[0-9]{5,25}$/.test(value)) {
      setIdError(
        "A Discord id is 5 to 25 digits. In Discord: Settings → Advanced → Developer Mode, then right-click the user → Copy User ID.",
      );
      return;
    }
    if ([...note].length > 200) return;
    setBusy(true);
    try {
      await addAllowlist(value, note);
      setId("");
      setNote("");
      setMessage(`${value} can now sign in.`);
      await client.invalidateQueries({ queryKey: serverKeys.allowlist });
    } catch (err) {
      if (err instanceof ApiError && err.code === "already_allowlisted")
        setIdError("This Discord id is already on the allowlist.");
      else if (err instanceof ApiError && err.fieldErrors().discord_id)
        setIdError(`The server says: ${err.fieldErrors().discord_id}.`);
      else setFailure(err);
    } finally {
      setBusy(false);
    }
  };

  if (list.error instanceof ApiError && list.error.status === 403) return <ForbiddenPage />;

  return (
    <section aria-labelledby="page-title">
      <h1 id="page-title">Allowlist</h1>
      <p className="muted">
        When registration is set to "allowlist", only these Discord accounts can create an account.
      </p>
      <form
        className="toolbar"
        onSubmit={(e) => void add(e)}
        aria-label="Add to the allowlist"
        noValidate
      >
        <div className="field">
          <label htmlFor="allow-id">Discord id</label>
          <input
            id="allow-id"
            className="mono"
            inputMode="numeric"
            value={id}
            aria-invalid={idError ? true : undefined}
            aria-describedby={idError ? "allow-id-error" : undefined}
            onChange={(e) => {
              setId(e.target.value);
              setIdError(null);
            }}
          />
          {idError ? (
            <span id="allow-id-error" className="field-error">
              {idError}
            </span>
          ) : null}
        </div>
        <div className="field">
          <label htmlFor="allow-note">Note (optional)</label>
          <input
            id="allow-note"
            dir="auto"
            value={note}
            aria-invalid={[...note].length > 200 || undefined}
            aria-describedby="allow-note-count"
            onChange={(e) => setNote(e.target.value)}
          />
          <span id="allow-note-count" className={[...note].length > 200 ? "field-error" : "muted"}>
            {[...note].length} / 200
          </span>
        </div>
        <button type="submit" className="primary" aria-disabled={busy || undefined}>
          {busy ? "Adding…" : "Add"}
        </button>
      </form>
      {message ? (
        <p className="banner ok" role="status">
          {message}
        </p>
      ) : null}
      {failure ? <ErrorView error={failure} /> : null}
      {list.isPending ? (
        <Loading label="Loading the allowlist…" />
      ) : list.isError ? (
        <ErrorView error={list.error} onRetry={() => void list.refetch()} />
      ) : list.data.length === 0 ? (
        <p role="status">Nobody is on the allowlist yet.</p>
      ) : (
        <div className="table-wrap">
          <table>
            <caption className="visually-hidden">Allowlisted Discord ids</caption>
            <thead>
              <tr>
                <th scope="col">Discord id</th>
                <th scope="col">Note</th>
                <th scope="col">Added by</th>
                <th scope="col">Added</th>
                <th scope="col">Actions</th>
              </tr>
            </thead>
            <tbody>
              {list.data.map((e) => (
                <tr key={e.discord_id}>
                  <th scope="row" className="mono">
                    {e.discord_id}
                  </th>
                  <td dir="auto">{e.note ?? ""}</td>
                  <td>{e.added_by?.username ?? "—"}</td>
                  <td>{when(e.created_at)}</td>
                  <td>
                    <button
                      type="button"
                      className="danger"
                      onClick={() => setRemoving(e.discord_id)}
                    >
                      Remove {e.discord_id}…
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {removing ? (
        <Modal title={`Remove ${removing}?`} role="alertdialog" onClose={() => setRemoving(null)}>
          <p>They can't create an account anymore. An account they already have keeps working.</p>
          <div className="actions">
            <button type="button" data-autofocus="" onClick={() => setRemoving(null)}>
              Cancel
            </button>
            <button
              type="button"
              className="danger"
              onClick={async () => {
                const target = removing;
                setRemoving(null);
                try {
                  await removeAllowlist(target);
                  setMessage(`${target} was removed.`);
                } catch (err) {
                  setFailure(err);
                }
                await client.invalidateQueries({ queryKey: serverKeys.allowlist });
              }}
            >
              Remove
            </button>
          </div>
        </Modal>
      ) : null}
    </section>
  );
}
