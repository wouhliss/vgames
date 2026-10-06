// Compatibility (A3-T16, 09-compatibility §4): an editor for the Linux (Proton) and macOS (Wine)
// profiles, with the ProtonDB tier and umu id as hints. Saving builds the vgames.compat/1 document,
// hashes it in the pack worker, signs it in the key worker with the publisher key, and PUTs the next
// revision. The current revision is shown read-only.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { compatKeys, compatProfiles, putCompatProfile, serverInfo } from "../../api/compat";
import { ApiError } from "../../api/errors";
import { listCandidates, metadataKeys } from "../../api/packages";
import { type CompatStatus, CompatStatusSchema, type SignedCompatProfile } from "../../api/schemas";
import { FINALIZE_ERRORS } from "../../api/versions";
import { ErrorView, Loading } from "../../app/ErrorView";
import { useUploadDeps } from "../../upload/context";
import type { KeyChannel, PackChannel } from "../../upload/rpc";
import { CompatHistory } from "./CompatHistory";
import {
  buildDocument,
  type CompatForm,
  emptyForm,
  type FormErrors,
  formFromDocument,
  fromBase64,
  GRAPHICS,
  type Target,
  toBase64,
  validate,
  WINETRICKS,
} from "./compatModel";
import { usePackageContext } from "./PackageLayout";
import { KeyStep } from "./UploadWizard";

const STATUS_LABEL: Record<CompatStatus, string> = {
  verified: "Verified",
  playable: "Playable",
  unsupported: "Unsupported",
  untested: "Untested",
};

export function CompatTab() {
  const { response } = usePackageContext();
  const pkg = response.data;
  const deps = useUploadDeps();
  const packs = useRef<PackChannel | null>(null);
  const keys = useRef<KeyChannel | null>(null);
  packs.current ??= deps.workers.pack();
  keys.current ??= deps.workers.key();
  useEffect(
    () => () => {
      packs.current?.terminate();
      keys.current?.terminate();
      packs.current = null;
      keys.current = null;
    },
    [],
  );
  const [key, setKey] = useState<{ keyId: string; label: string } | null>(null);

  const profiles = useQuery({
    queryKey: compatKeys.profiles(pkg.id),
    queryFn: async ({ signal }) => {
      try {
        return (await compatProfiles(pkg.id, signal)).data.items;
      } catch (e) {
        // Unpublished packages aren't in the catalog yet: no profiles.
        if (e instanceof ApiError && e.status === 404) return [];
        throw e;
      }
    },
  });
  const server = useQuery({
    queryKey: compatKeys.server,
    queryFn: async ({ signal }) => (await serverInfo(signal)).data,
  });
  const candidates = useQuery({
    queryKey: metadataKeys.candidates(pkg.id),
    queryFn: async ({ signal }) => (await listCandidates(pkg.id, signal)).data,
  });
  const umuHint = candidates.data?.items.find((c) => c.data.external?.umu_id)?.data.external
    ?.umu_id;

  if (profiles.isPending || server.isPending)
    return <Loading label="Loading compatibility profiles…" />;
  if (profiles.isError)
    return <ErrorView error={profiles.error} onRetry={() => void profiles.refetch()} />;
  if (server.isError)
    return <ErrorView error={server.error} onRetry={() => void server.refetch()} />;

  return (
    <div>
      <p>
        How Windows builds of this package run on Linux (Proton) and macOS (Wine). Each save
        publishes a new signed revision; launchers pick it up without a new upload.
      </p>
      <section aria-labelledby="compat-key">
        <h2 id="compat-key">Publisher key</h2>
        <KeyStep keys={keys.current} unlocked={key} onUnlocked={setKey} />
      </section>
      {(["linux", "macos"] as const).map((target) => (
        <TargetEditor
          key={target}
          target={target}
          packageId={pkg.id}
          serverId={server.data.server_id}
          current={profiles.data.find((p) => p.target === target) ?? null}
          protondb={pkg.protondb_tier}
          umuHint={umuHint}
          packs={packs.current}
          keys={keys.current}
          unlocked={key !== null}
        />
      ))}
    </div>
  );
}

function TargetEditor({
  target,
  packageId,
  serverId,
  current,
  protondb,
  umuHint,
  packs,
  keys,
  unlocked,
}: {
  target: Target;
  packageId: string;
  serverId: string;
  current: SignedCompatProfile | null;
  protondb: string | undefined;
  umuHint: string | undefined;
  packs: PackChannel | null;
  keys: KeyChannel | null;
  unlocked: boolean;
}) {
  const client = useQueryClient();
  const currentDoc = (() => {
    if (!current) return null;
    try {
      return JSON.parse(new TextDecoder().decode(fromBase64(current.document))) as unknown;
    } catch {
      return null;
    }
  })();
  const [form, setForm] = useState<CompatForm>(() =>
    currentDoc ? formFromDocument(currentDoc, target) : emptyForm(target),
  );
  const [errors, setErrors] = useState<FormErrors>({});
  const [failure, setFailure] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const id = (f: string) => `${target}-${f}`;
  const set = <K extends keyof CompatForm>(k: K, v: CompatForm[K]) => {
    setForm((prev) => ({ ...prev, [k]: v }));
    setErrors((e) => ({ ...e, [k]: undefined }));
    setSaved(null);
  };
  const next = (current?.revision ?? 0) + 1;

  const save = async () => {
    if (busy || !packs || !keys) return;
    setFailure(null);
    setSaved(null);
    const found = validate(form, target);
    setErrors(found);
    if (Object.values(found).some(Boolean)) return;
    setBusy(true);
    try {
      const bytes = buildDocument(form, {
        serverId,
        packageId,
        target,
        revision: next,
        now: new Date(),
      });
      const hashed = await packs.call({ kind: "hash", bytes });
      if (hashed.kind !== "hash")
        throw new Error(hashed.kind === "error" ? hashed.message : "hash failed");
      const signed = await keys.call({ kind: "sign", digest: hashed.blake3, context: "compat" });
      if (signed.kind !== "signed")
        throw new Error(signed.kind === "error" ? signed.message : "signing failed");
      await putCompatProfile(packageId, target, {
        document: toBase64(bytes),
        signature: JSON.parse(signed.envelope),
      });
      setSaved(`Revision ${next} is published.`);
      await Promise.all([
        client.invalidateQueries({ queryKey: compatKeys.profiles(packageId) }),
        client.invalidateQueries({ queryKey: compatKeys.history(packageId) }),
      ]);
    } catch (e) {
      if (e instanceof ApiError && e.status === 409)
        setFailure(
          "Someone published a newer revision meanwhile. Reload the page, check it, then save again.",
        );
      else if (e instanceof ApiError && e.status === 422)
        setFailure(
          FINALIZE_ERRORS[e.code ?? ""] ??
            `The server refused the profile: ${e.detail.kind === "http" ? (e.detail.problem.detail ?? e.detail.problem.title) : e.message}`,
        );
      else if (e instanceof ApiError) setFailure(e.message);
      else setFailure(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const title = target === "linux" ? "Linux (Proton)" : "macOS (Wine)";
  return (
    <section aria-labelledby={id("title")} className="form-grid">
      <h2 id={id("title")}>{title}</h2>
      {current ? (
        <p className="muted">
          Current: revision {current.revision}, {STATUS_LABEL[current.status]}, signed by key{" "}
          <span className="mono">{current.signature.key_id}</span> on{" "}
          {new Date(current.created_at).toLocaleString()}.
        </p>
      ) : (
        <p className="muted">No profile yet: launchers use the defaults (untested).</p>
      )}
      <CompatHistory packageId={packageId} target={target} />
      {target === "linux" && protondb ? (
        <p className="muted">Hint: ProtonDB rates it {protondb}.</p>
      ) : null}
      <div className="field">
        <label htmlFor={id("status")}>Status</label>
        <select
          id={id("status")}
          value={form.status}
          onChange={(e) => set("status", e.target.value as CompatStatus)}
        >
          {CompatStatusSchema.options.map((s) => (
            <option key={s} value={s}>
              {STATUS_LABEL[s]}
            </option>
          ))}
        </select>
      </div>
      <Text
        id={id("notes")}
        label="Notes for players (plain text)"
        area
        value={form.notes}
        error={errors.notes}
        onChange={(v) => set("notes", v)}
        count={2000}
      />
      <div className="toolbar">
        <div className="field">
          <label htmlFor={id("platform")}>Windows build</label>
          <select
            id={id("platform")}
            value={form.platform}
            onChange={(e) => set("platform", e.target.value as CompatForm["platform"])}
          >
            <option value="windows-x86_64">windows-x86_64</option>
            <option value="windows-aarch64">windows-aarch64</option>
          </select>
        </div>
        <Text
          id={id("min")}
          label="From version #"
          value={form.minSequence}
          error={errors.minSequence}
          onChange={(v) => set("minSequence", v)}
        />
        <Text
          id={id("max")}
          label="Up to version # (optional)"
          value={form.maxSequence}
          error={errors.maxSequence}
          onChange={(v) => set("maxSequence", v)}
        />
      </div>
      <Text
        id={id("prefer")}
        label="Preferred runtimes (ids, most preferred first, comma separated)"
        value={form.prefer}
        error={errors.prefer}
        onChange={(v) => set("prefer", v)}
        mono
      />
      <Text
        id={id("minver")}
        label="Minimum runtime version (optional)"
        value={form.minVersion}
        error={errors.minVersion}
        onChange={(v) => set("minVersion", v)}
        mono
      />
      {target === "linux" ? (
        <Text
          id={id("umu")}
          label="umu game id (optional)"
          hint={umuHint ? `Hint from the umu database: ${umuHint}` : undefined}
          value={form.umuGameId}
          error={errors.umuGameId}
          onChange={(v) => set("umuGameId", v)}
          mono
        />
      ) : (
        <fieldset>
          <legend>Graphics backends, in order of preference</legend>
          {GRAPHICS.map((g) => {
            const at = form.graphics.indexOf(g);
            return (
              <label key={g} className="row">
                <input
                  type="checkbox"
                  checked={at >= 0}
                  onChange={(e) =>
                    set(
                      "graphics",
                      e.target.checked
                        ? [...form.graphics, g]
                        : form.graphics.filter((x) => x !== g),
                    )
                  }
                />{" "}
                {g}
                {at >= 0 ? <span className="muted"> (#{at + 1})</span> : null}
              </label>
            );
          })}
        </fieldset>
      )}
      <Text
        id={id("env")}
        label="Environment (NAME=value per line)"
        area
        value={form.env}
        error={errors.env}
        onChange={(v) => set("env", v)}
        mono
      />
      <Text
        id={id("dll")}
        label="DLL overrides (name=mode per line)"
        area
        value={form.dllOverrides}
        error={errors.dllOverrides}
        onChange={(v) => set("dllOverrides", v)}
        mono
      />
      <fieldset>
        <legend>Winetricks verbs (only the allowed ones)</legend>
        <div className="row">
          {WINETRICKS.map((verb) => (
            <label key={verb}>
              <input
                type="checkbox"
                checked={form.winetricks.includes(verb)}
                onChange={(e) =>
                  set(
                    "winetricks",
                    e.target.checked
                      ? [...form.winetricks, verb]
                      : form.winetricks.filter((v) => v !== verb),
                  )
                }
              />{" "}
              <span className="mono">{verb}</span>
            </label>
          ))}
        </div>
        {errors.winetricks ? <span className="field-error">{errors.winetricks}</span> : null}
      </fieldset>
      {failure ? (
        <p className="banner error" role="alert">
          {failure}
        </p>
      ) : null}
      {saved ? (
        <p className="banner ok" role="status">
          {saved}
        </p>
      ) : null}
      <button
        type="button"
        className="primary"
        aria-disabled={!unlocked || busy || undefined}
        onClick={() => void (unlocked ? save() : undefined)}
      >
        {busy ? "Signing…" : `Sign and publish revision ${next}`}
      </button>
      {!unlocked ? <p className="muted">Unlock your publisher key above to sign.</p> : null}
    </section>
  );
}

function Text({
  id,
  label,
  value,
  error,
  onChange,
  area,
  mono,
  hint,
  count,
}: {
  id: string;
  label: string;
  value: string;
  error: string | undefined;
  onChange: (v: string) => void;
  area?: boolean;
  mono?: boolean;
  hint?: string | undefined;
  count?: number;
}) {
  const described =
    [hint && `${id}-hint`, error && `${id}-error`].filter(Boolean).join(" ") || undefined;
  const common = {
    id,
    value,
    className: mono ? "mono" : undefined,
    "aria-invalid": error ? true : undefined,
    "aria-describedby": described,
  } as const;
  return (
    <div className="field">
      <label htmlFor={id}>{label}</label>
      {area ? (
        <textarea {...common} rows={3} dir="auto" onChange={(e) => onChange(e.target.value)} />
      ) : (
        <input {...common} onChange={(e) => onChange(e.target.value)} />
      )}
      {hint ? (
        <span id={`${id}-hint`} className="muted">
          {hint}
        </span>
      ) : null}
      {count ? (
        <span className={[...value].length > count ? "field-error" : "muted"}>
          {[...value].length} / {count.toLocaleString("en")}
        </span>
      ) : null}
      {error ? (
        <span id={`${id}-error`} className="field-error">
          {error}
        </span>
      ) : null}
    </div>
  );
}
