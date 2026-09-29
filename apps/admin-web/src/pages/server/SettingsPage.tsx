// Server settings (A3-T17): name, message of the day, registration mode. Owners edit (If-Match);
// admins see them read-only. A 412 keeps the input and shows what changed.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { ApiError } from "../../api/errors";
import type { ApiResponse } from "../../api/http";
import type { ServerSettings } from "../../api/schemas";
import { getSettings, serverKeys, updateSettings } from "../../api/server";
import { ErrorView, ForbiddenPage, Loading } from "../../app/ErrorView";
import { useRole } from "./shared";

const MODES = [
  { value: "open", label: "Open", text: "Anyone with a Discord account can join." },
  {
    value: "allowlist",
    label: "Allowlist",
    text: "Only Discord accounts on the allowlist can join.",
  },
  {
    value: "closed",
    label: "Closed",
    text: "Nobody new can join. Existing accounts keep working.",
  },
] as const;

export function SettingsPage() {
  const settings = useQuery({
    queryKey: serverKeys.settings,
    queryFn: ({ signal }) => getSettings(signal),
  });
  if (settings.isPending) return <Loading label="Loading settings…" />;
  if (settings.isError) {
    if (settings.error instanceof ApiError && settings.error.status === 403)
      return <ForbiddenPage />;
    return <ErrorView error={settings.error} onRetry={() => void settings.refetch()} />;
  }
  // The form keeps its own copy and ETag once loaded (saves update both).
  return <SettingsForm initial={settings.data} />;
}

function SettingsForm({ initial }: { initial: ApiResponse<ServerSettings> }) {
  const role = useRole();
  const client = useQueryClient();
  const readOnly = role !== "owner";
  const [etag, setEtag] = useState(initial.etag ?? "");
  const [base, setBase] = useState(initial.data);
  const [name, setName] = useState(initial.data.name);
  const [motd, setMotd] = useState(initial.data.motd ?? "");
  const [mode, setMode] = useState(initial.data.registration_mode);
  const [errors, setErrors] = useState<{ name?: string | undefined; motd?: string | undefined }>(
    {},
  );
  const [failure, setFailure] = useState<unknown>(null);
  const [conflict, setConflict] = useState<ApiResponse<ServerSettings> | null>(null);
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);

  const save = async (e: FormEvent) => {
    e.preventDefault();
    if (readOnly || busy) return;
    setSaved(false);
    setFailure(null);
    const local: typeof errors = {};
    const n = [...name.trim()].length;
    if (n < 1 || n > 100) local.name = "1 to 100 characters.";
    if ([...motd].length > 500) local.motd = "At most 500 characters.";
    setErrors(local);
    if (Object.keys(local).length) return;
    const patch: Parameters<typeof updateSettings>[1] = {};
    if (name.trim() !== base.name) patch.name = name.trim();
    if (motd !== (base.motd ?? "")) patch.motd = motd;
    if (mode !== base.registration_mode) patch.registration_mode = mode;
    if (Object.keys(patch).length === 0) {
      setSaved(true);
      return;
    }
    setBusy(true);
    try {
      const result = await updateSettings(etag, patch);
      setEtag(result.etag ?? "");
      setBase(result.data);
      setConflict(null);
      setSaved(true);
      client.setQueryData(serverKeys.settings, result);
    } catch (err) {
      if (err instanceof ApiError && err.status === 412) setConflict(await getSettings());
      else if (err instanceof ApiError && err.status === 400) {
        const f = err.fieldErrors();
        setErrors({ name: f.name, motd: f.motd });
      } else setFailure(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section aria-labelledby="page-title" className="form-grid">
      <h1 id="page-title">Server settings</h1>
      {readOnly ? <p className="banner warn">Only an owner can change these settings.</p> : null}
      {failure ? <ErrorView error={failure} /> : null}
      {conflict ? (
        <div className="banner warn" role="alert">
          <strong>Changed by someone else</strong>
          <p>
            Saved now: name "{conflict.data.name}", registration {conflict.data.registration_mode},
            message "{conflict.data.motd ?? ""}". Your input is still in the form.
          </p>
          <div className="row">
            <button
              type="button"
              onClick={() => {
                // Fields I didn't touch take their saved value; my edits stay.
                const theirs = conflict.data;
                if (name.trim() === base.name) setName(theirs.name);
                if (motd === (base.motd ?? "")) setMotd(theirs.motd ?? "");
                if (mode === base.registration_mode) setMode(theirs.registration_mode);
                setEtag(conflict.etag ?? "");
                setBase(theirs);
                setConflict(null);
              }}
            >
              Keep my changes (then save)
            </button>
            <button
              type="button"
              onClick={() => {
                setEtag(conflict.etag ?? "");
                setBase(conflict.data);
                setName(conflict.data.name);
                setMotd(conflict.data.motd ?? "");
                setMode(conflict.data.registration_mode);
                setConflict(null);
              }}
            >
              Discard mine and load theirs
            </button>
          </div>
        </div>
      ) : null}
      <form onSubmit={(e) => void save(e)} noValidate aria-label="Server settings">
        <div className="field">
          <label htmlFor="set-name">Server name (required)</label>
          <input
            id="set-name"
            dir="auto"
            value={name}
            readOnly={readOnly}
            aria-invalid={errors.name ? true : undefined}
            aria-describedby={errors.name ? "set-name-error" : undefined}
            onChange={(e) => setName(e.target.value)}
          />
          {errors.name ? (
            <span id="set-name-error" className="field-error">
              {errors.name}
            </span>
          ) : null}
        </div>
        <div className="field">
          <label htmlFor="set-motd">Message of the day</label>
          <textarea
            id="set-motd"
            dir="auto"
            rows={3}
            value={motd}
            readOnly={readOnly}
            aria-invalid={errors.motd ? true : undefined}
            aria-describedby="set-motd-count"
            onChange={(e) => setMotd(e.target.value)}
          />
          <span id="set-motd-count" className={[...motd].length > 500 ? "field-error" : "muted"}>
            {[...motd].length} / 500 {errors.motd ?? ""}
          </span>
        </div>
        <fieldset disabled={readOnly}>
          <legend>Registration</legend>
          {MODES.map((m) => (
            <label key={m.value} className="field">
              <span>
                <input
                  type="radio"
                  name="registration"
                  value={m.value}
                  checked={mode === m.value}
                  onChange={() => setMode(m.value)}
                />{" "}
                {m.label}
              </span>
              <span className="muted">{m.text}</span>
            </label>
          ))}
        </fieldset>
        {readOnly ? null : (
          <button type="submit" className="primary" aria-disabled={busy || undefined}>
            {busy ? "Saving…" : "Save settings"}
          </button>
        )}
        <p role="status" className="muted">
          {saved ? "Saved." : ""}
        </p>
      </form>
    </section>
  );
}
