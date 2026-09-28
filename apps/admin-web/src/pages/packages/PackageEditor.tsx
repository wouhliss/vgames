// The package editor. Saves send only what changed, as a JSON Merge Patch with `If-Match`. When
// someone else saved in between (412), the user's input is kept and the differences are shown side
// by side, with "save mine over theirs" or "load theirs". Leaving with unsaved changes asks first
// (in-app navigation and tab close). Status changes and delete are confirmed; delete needs the slug
// typed. No optimistic updates: the page shows what the server confirmed.
import { useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useEffect, useRef, useState } from "react";
import { useBlocker, useNavigate } from "react-router";
import { ApiError } from "../../api/errors";
import type { ApiResponse } from "../../api/http";
import { deletePackage, getPackage, packageKeys, updatePackage } from "../../api/packages";
import type { AdminPackage, PackageStatus } from "../../api/schemas";
import { ErrorView } from "../../app/ErrorView";
import { Field } from "../../components/Field";
import { Modal } from "../../components/Modal";
import {
  buildPatch,
  conflicts,
  FIELDS,
  type FieldName,
  type FieldSpec,
  type FormValues,
  fromPackage,
  isDirty,
  length,
  SOURCE_LABEL,
  STATUS_LABEL,
  STATUSES,
  validateAll,
} from "./model";
import { usePackageContext } from "./PackageLayout";

const STATUS_EFFECT: Record<PackageStatus, string> = {
  draft: "Players can't see it. Use this while you prepare the package.",
  published: "Players on this server can find, install and update it.",
  hidden:
    "It disappears from the catalog. Players who installed it keep playing, but can't reinstall it.",
  archived:
    "It disappears from the catalog and admin lists default to hiding it. Installs keep working.",
};

/** Maps server field paths (`genres[3]`) onto form fields. */
function toFormErrors(errors: Record<string, string>): Partial<Record<FieldName, string>> {
  const out: Partial<Record<FieldName, string>> = {};
  for (const [path, message] of Object.entries(errors)) {
    const name = path.replace(/\[\d+\]$/, "") as FieldName;
    if (FIELDS.some((f) => f.name === name)) out[name] ??= `The server says: ${message}.`;
  }
  return out;
}

/** The Details tab: the form, status and delete. */
export function PackageEditor() {
  const { response } = usePackageContext();
  return <Editor key={response.data.id} initial={response} />;
}

function Editor({ initial }: { initial: ApiResponse<AdminPackage> }) {
  const client = useQueryClient();
  const navigate = useNavigate();
  const [pkg, setPkg] = useState(initial.data);
  const [etag, setEtag] = useState(initial.etag ?? "");
  const [base, setBase] = useState(() => fromPackage(initial.data));
  const [values, setValues] = useState(base);
  const [errors, setErrors] = useState<Partial<Record<FieldName, string>>>({});
  const [failure, setFailure] = useState<unknown>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [conflict, setConflict] = useState<{ latest: AdminPackage; etag: string } | null>(null);
  const [statusChoice, setStatusChoice] = useState<PackageStatus>(initial.data.status);
  const [confirmStatus, setConfirmStatus] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const formRef = useRef<HTMLFormElement>(null);
  const dirty = isDirty(base, values);

  // Tab close / reload with unsaved changes.
  useEffect(() => {
    if (!dirty) return;
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    return () => window.removeEventListener("beforeunload", onBeforeUnload);
  }, [dirty]);

  // In-app navigation with unsaved changes.
  const blocker = useBlocker(
    ({ currentLocation, nextLocation }) =>
      dirty && currentLocation.pathname !== nextLocation.pathname,
  );

  const accept = (response: ApiResponse<AdminPackage>, message: string) => {
    setPkg(response.data);
    setEtag(response.etag ?? "");
    const next = fromPackage(response.data);
    setBase(next);
    setValues(next);
    setStatusChoice(response.data.status);
    client.setQueryData(packageKeys.one(response.data.id), response);
    void client.invalidateQueries({ queryKey: ["admin-packages"] });
    setSaved(message);
  };

  const focusFirstError = () => {
    requestAnimationFrame(() =>
      formRef.current?.querySelector<HTMLElement>("[aria-invalid='true']")?.focus(),
    );
  };

  /** A 412: fetch what is saved now and show it next to the user's input. */
  const loadConflict = async () => {
    try {
      const latest = await getPackage(pkg.id);
      setConflict({ latest: latest.data, etag: latest.etag ?? "" });
    } catch (error) {
      setFailure(error);
    }
  };

  const save = async (e?: FormEvent) => {
    e?.preventDefault();
    if (saving) return;
    setSaved(null);
    setFailure(null);
    const local = validateAll(values);
    setErrors(local);
    if (Object.keys(local).length > 0) {
      focusFirstError();
      return;
    }
    const patch = buildPatch(base, values);
    if (Object.keys(patch).length === 0) {
      setSaved("No changes to save.");
      return;
    }
    setSaving(true);
    try {
      accept(await updatePackage(pkg.id, etag, patch), "Saved.");
      setConflict(null);
    } catch (error) {
      if (error instanceof ApiError && error.status === 412) await loadConflict();
      else if (error instanceof ApiError && error.status === 400) {
        const mapped = toFormErrors(error.fieldErrors());
        if (Object.keys(mapped).length > 0) {
          setErrors(mapped);
          focusFirstError();
        } else setFailure(error);
      } else if (error instanceof ApiError && error.code === "slug_taken") {
        setErrors({ slug: "Another package already uses this slug. Pick a different one." });
        focusFirstError();
      } else setFailure(error);
    } finally {
      setSaving(false);
    }
  };

  const changeStatus = async () => {
    setSaved(null);
    setFailure(null);
    try {
      const response = await updatePackage(pkg.id, etag, { status: statusChoice });
      // Keep unsaved edits: only the status and the version tag move on.
      setPkg(response.data);
      setEtag(response.etag ?? "");
      client.setQueryData(packageKeys.one(response.data.id), response);
      void client.invalidateQueries({ queryKey: ["admin-packages"] });
      setSaved(`Status changed to ${STATUS_LABEL[response.data.status]}.`);
    } catch (error) {
      if (error instanceof ApiError && error.status === 412) await loadConflict();
      else setFailure(error);
      setStatusChoice(pkg.status);
    } finally {
      setConfirmStatus(false);
    }
  };

  const remove = async () => {
    try {
      await deletePackage(pkg.id, etag);
      client.removeQueries({ queryKey: packageKeys.one(pkg.id) });
      await client.invalidateQueries({ queryKey: ["admin-packages"] });
      // Nothing is left to protect: skip the unsaved-changes question.
      setBase(values);
      navigate("/packages", { replace: true, state: { deleted: pkg.title } });
    } catch (error) {
      setConfirmDelete(false);
      if (error instanceof ApiError && error.status === 412) await loadConflict();
      else setFailure(error);
    }
  };

  const set = (name: FieldName, value: string) => {
    setValues((v) => ({ ...v, [name]: value }));
    setSaved(null);
    if (errors[name]) setErrors((e) => ({ ...e, [name]: undefined }));
  };

  return (
    <div className="form-grid">
      {failure ? <ErrorView error={failure} /> : null}
      {conflict ? (
        <ConflictPanel
          started={base}
          latest={fromPackage(conflict.latest)}
          mine={values}
          onOverwrite={() => {
            // Save over theirs: the new base is what's saved now, so only my differences are sent.
            setBase(fromPackage(conflict.latest));
            setPkg(conflict.latest);
            setEtag(conflict.etag);
            setConflict(null);
            setSaved("Their version is loaded underneath your changes. Review, then save.");
          }}
          onDiscard={() => {
            const next = fromPackage(conflict.latest);
            setPkg(conflict.latest);
            setEtag(conflict.etag);
            setBase(next);
            setValues(next);
            setStatusChoice(conflict.latest.status);
            setConflict(null);
            setSaved("Loaded the latest saved version.");
          }}
        />
      ) : null}
      <p role="status" className="muted">
        {saved ?? (dirty ? "You have unsaved changes." : "")}
      </p>

      <form ref={formRef} onSubmit={(e) => void save(e)} noValidate aria-label="Package details">
        {FIELDS.map((spec) => (
          <FormField
            key={spec.name}
            spec={spec}
            value={values[spec.name]}
            error={errors[spec.name]}
            source={pkg.field_sources[spec.name]}
            onChange={(v) => set(spec.name, v)}
          />
        ))}
        <div className="row">
          <button type="submit" className="primary" aria-disabled={saving || undefined}>
            {saving ? "Saving…" : "Save changes"}
          </button>
          <button
            type="button"
            disabled={!dirty || saving}
            onClick={() => {
              setValues(base);
              setErrors({});
            }}
          >
            Undo changes
          </button>
        </div>
      </form>

      <h2>Status</h2>
      <div className="toolbar">
        <div className="field">
          <label htmlFor="status-choice">New status</label>
          <select
            id="status-choice"
            value={statusChoice}
            onChange={(e) => setStatusChoice(e.target.value as PackageStatus)}
          >
            {STATUSES.map((s) => (
              <option key={s} value={s}>
                {STATUS_LABEL[s]}
              </option>
            ))}
          </select>
        </div>
        <button
          type="button"
          disabled={statusChoice === pkg.status}
          onClick={() => setConfirmStatus(true)}
        >
          Change status…
        </button>
      </div>

      <h2>Delete</h2>
      <p>
        Deleting hides the package everywhere. Players who installed it can keep playing offline.
      </p>
      <button type="button" className="danger" onClick={() => setConfirmDelete(true)}>
        Delete package…
      </button>

      {confirmStatus ? (
        <Modal
          title={`Change status to ${STATUS_LABEL[statusChoice]}?`}
          onClose={() => setConfirmStatus(false)}
        >
          <p>
            From {STATUS_LABEL[pkg.status]} to {STATUS_LABEL[statusChoice]}.{" "}
            {STATUS_EFFECT[statusChoice]}
          </p>
          <div className="actions">
            <button type="button" onClick={() => setConfirmStatus(false)}>
              Cancel
            </button>
            <button type="button" className="primary" onClick={() => void changeStatus()}>
              Change to {STATUS_LABEL[statusChoice]}
            </button>
          </div>
        </Modal>
      ) : null}
      {confirmDelete ? (
        <DeleteDialog
          slug={pkg.slug}
          title={pkg.title}
          onCancel={() => setConfirmDelete(false)}
          onConfirm={remove}
        />
      ) : null}
      {blocker.state === "blocked" ? (
        <Modal title="Leave without saving?" role="alertdialog" onClose={() => blocker.reset()}>
          <p>Your changes to this package haven't been saved and will be lost.</p>
          <div className="actions">
            <button type="button" data-autofocus="" onClick={() => blocker.reset()}>
              Stay on this page
            </button>
            <button type="button" className="danger" onClick={() => blocker.proceed()}>
              Leave and discard changes
            </button>
          </div>
        </Modal>
      ) : null}
    </div>
  );
}

function FormField({
  spec,
  value,
  error,
  source,
  onChange,
}: {
  spec: FieldSpec;
  value: string;
  error: string | undefined;
  source: string | undefined;
  onChange: (value: string) => void;
}) {
  const badge =
    source === "admin" || source === "igdb" || source === "steam" ? (
      <span className="badge">{SOURCE_LABEL[source]}</span>
    ) : null;
  const count =
    spec.max !== undefined && spec.kind !== "id"
      ? { value: length(spec.name === "title" ? value.trim() : value), max: spec.max }
      : undefined;
  const label = spec.required ? `${spec.label} (required)` : spec.label;
  return (
    <Field label={label} hint={spec.hint} error={error} count={count} badge={badge}>
      {(p) =>
        spec.kind === "textarea" || spec.kind === "genres" ? (
          <textarea
            {...p}
            dir="auto"
            rows={spec.name === "description" ? 10 : 3}
            value={value}
            onChange={(e) => onChange(e.target.value)}
          />
        ) : (
          <input
            {...p}
            dir={spec.name === "slug" ? "ltr" : "auto"}
            className={spec.name === "slug" ? "mono" : undefined}
            type={spec.kind === "date" ? "date" : "text"}
            inputMode={spec.kind === "id" ? "numeric" : undefined}
            required={spec.required}
            value={value}
            onChange={(e) => onChange(e.target.value)}
          />
        )
      }
    </Field>
  );
}

function ConflictPanel({
  started,
  latest,
  mine,
  onOverwrite,
  onDiscard,
}: {
  started: FormValues;
  latest: FormValues;
  mine: FormValues;
  onOverwrite: () => void;
  onDiscard: () => void;
}) {
  const rows = conflicts(started, latest, mine);
  return (
    <div className="banner warn" role="alert">
      <strong>Changed by someone else</strong>
      <p>
        Someone saved this package after you opened it. Your input is still in the form. Compare,
        then choose.
      </p>
      {rows.length > 0 ? (
        <table className="diff">
          <caption className="visually-hidden">Differences</caption>
          <thead>
            <tr>
              <th scope="col">Field</th>
              <th scope="col">Saved by them</th>
              <th scope="col">Yours</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.field.name}>
                <th scope="row">{r.field.label}</th>
                <td dir="auto">{r.theirs || "(empty)"}</td>
                <td dir="auto">{r.mine || "(empty)"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        <p>Only the status or images changed; none of the fields you can edit here.</p>
      )}
      <div className="row">
        <button type="button" onClick={onOverwrite}>
          Keep my changes (then save)
        </button>
        <button type="button" onClick={onDiscard}>
          Discard mine and load theirs
        </button>
      </div>
    </div>
  );
}

function DeleteDialog({
  slug,
  title,
  onCancel,
  onConfirm,
}: {
  slug: string;
  title: string;
  onCancel: () => void;
  onConfirm: () => Promise<void>;
}) {
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const matches = typed.trim() === slug;
  return (
    <Modal title={`Delete ${title}?`} role="alertdialog" busy={busy} onClose={onCancel}>
      <p>
        It disappears from the catalog and from this list. Players who installed it keep their
        files.
      </p>
      <form
        onSubmit={async (e) => {
          e.preventDefault();
          if (!matches || busy) return;
          setBusy(true);
          await onConfirm();
          setBusy(false);
        }}
      >
        <div className="field">
          <label htmlFor="delete-confirm">
            Type <span className="mono">{slug}</span> to confirm
          </label>
          <input
            id="delete-confirm"
            className="mono"
            autoComplete="off"
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
          />
        </div>
        <div className="actions">
          <button type="button" onClick={onCancel} disabled={busy}>
            Cancel
          </button>
          <button type="submit" className="danger" disabled={!matches || busy}>
            {busy ? "Deleting…" : "Delete package"}
          </button>
        </div>
      </form>
    </Modal>
  );
}
