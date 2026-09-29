// Browser upload of a new version (A3-T16, 02-package-format §6), step by step:
//  1. platform and version label (new uploads) — or the version being resumed;
//  2. the folder: paths checked (invalid ones listed, blocking), the plan summarized; on resume the
//     re-picked folder must be unchanged (sizes and modification times);
//  3. what the launcher starts (an executable from the folder, optional arguments);
//  4. the publisher key file and passphrase, unlocked only inside the key worker;
//  5. the upload: per-pack progress, pause/resume, automatic resume after a network loss, then
//     signing, finalize (422s explained), server verification, and publish.
// One tab per upload (Web Locks); leaving mid-upload asks first; progress survives a reload.
import { useQueryClient } from "@tanstack/react-query";
import {
  type FormEvent,
  type ReactNode,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { Link, useBlocker, useParams } from "react-router";
import { ApiError } from "../../api/errors";
import { newIdempotencyKey } from "../../api/http";
import { type Platform, PlatformSchema, type Version } from "../../api/schemas";
import { createVersion, getVersion, publishVersion, versionKeys } from "../../api/versions";
import { ErrorView, Loading } from "../../app/ErrorView";
import { Modal } from "../../components/Modal";
import { VirtualList } from "../../components/VirtualList";
import { useUploadDeps } from "../../upload/context";
import { type Snapshot, UploadEngine } from "../../upload/engine";
import {
  canPickDirectory,
  fromFileList,
  type PickedFolder,
  pickDirectory,
} from "../../upload/folder";
import { acquireUploadLock, type Release } from "../../upload/lock";
import type { KeyChannel, PackChannel } from "../../upload/rpc";
import { folderChanges, type SavedUpload } from "../../upload/store";
import type { Execution, PlanSummary } from "../../upload/types";
import { usePackageContext } from "./PackageLayout";
import { formatSize } from "./VersionsTab";

const PLATFORMS = PlatformSchema.options;

/** Files the launcher can start (a hint; any file may be chosen). */
const LAUNCHABLE = /\.(exe|bat|cmd|sh|x86_64|x86|appimage|command)$|^[^.]+$/i;

function Step({
  n,
  title,
  enabled,
  children,
}: {
  n: number;
  title: string;
  enabled: boolean;
  children: ReactNode;
}) {
  return (
    <section aria-labelledby={`step-${n}`} aria-disabled={!enabled || undefined}>
      <h2 id={`step-${n}`}>
        {n}. {title}
      </h2>
      {enabled ? children : <p className="muted">Complete the step above first.</p>}
    </section>
  );
}

export function UploadWizard() {
  const { response } = usePackageContext();
  const pkg = response.data;
  const resumeId = useParams().versionId ?? null;
  const deps = useUploadDeps();
  const client = useQueryClient();

  // Workers live as long as the page.
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

  const [version, setVersion] = useState<Version | null>(null);
  const [saved, setSaved] = useState<SavedUpload | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [folder, setFolder] = useState<PickedFolder | null>(null);
  const [plan, setPlan] = useState<PlanSummary | null>(null);
  const [execution, setExecution] = useState<Execution | undefined | null>(null);
  const [keyInfo, setKeyInfo] = useState<{ keyId: string; label: string } | null>(null);
  const [engine, setEngine] = useState<UploadEngine | null>(null);

  // Resuming: the version and what was saved for it.
  useEffect(() => {
    if (!resumeId) return;
    let cancelled = false;
    (async () => {
      try {
        const { data } = await getVersion(resumeId);
        const stored = await deps.store().get(resumeId);
        if (cancelled) return;
        setVersion(data);
        setSaved(stored);
        if (stored) setExecution(stored.execution);
      } catch (e) {
        if (!cancelled) setLoadError(e);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [resumeId, deps]);

  if (resumeId && !version && !loadError) return <Loading label="Loading the upload…" />;
  if (loadError) return <ErrorView error={loadError} />;
  if (version && version.state !== "uploading" && !engine)
    return (
      <div>
        <p role="status">
          Version {version.version_label} is no longer uploading (it is {version.state}).{" "}
          <Link to={`/packages/${pkg.id}/versions`}>Back to versions</Link>
        </p>
      </div>
    );

  const planned = plan !== null && folder !== null;
  const launchReady = execution !== null;

  const startUpload = async () => {
    if (
      !version ||
      !plan ||
      !folder ||
      execution === null ||
      !keyInfo ||
      !packs.current ||
      !keys.current
    )
      return;
    const base: SavedUpload = saved ?? {
      versionId: version.id,
      packageId: pkg.id,
      serverId: version.server_id,
      platform: version.platform,
      label: version.version_label,
      sequence: version.sequence,
      createdAt: Math.floor(Date.parse(version.created_at) / 1000),
      files: plan.files,
      directories: folder.directories,
      packs: plan.packs,
      execution,
      sessions: {},
      confirmed: {},
      complete: [],
      savedAt: new Date().toISOString(),
    };
    base.execution = execution;
    await deps.store().put(base);
    setSaved(base);
    setEngine(
      new UploadEngine(base, { packs: packs.current, keys: keys.current, store: deps.store() }),
    );
  };

  return (
    <div className="steps">
      <p>
        <Link to={`/packages/${pkg.id}/versions`}>← Versions</Link>
      </p>
      <Step n={1} title="Version" enabled>
        {version ? (
          <p>
            Version <strong dir="auto">{version.version_label}</strong> for{" "}
            <span className="mono">{version.platform}</span> (#{version.sequence}).
            {saved ? " Pick the same folder again to continue where the upload stopped." : ""}
          </p>
        ) : (
          <CreateVersion packageId={pkg.id} onCreated={setVersion} />
        )}
      </Step>
      <Step n={2} title="Folder" enabled={version !== null && !engine}>
        <FolderStep
          packs={packs.current}
          saved={saved}
          mockPackSize={deps.mockPackSize}
          onPlanned={(f, p) => {
            setFolder(f);
            setPlan(p);
          }}
          onCleared={() => {
            setFolder(null);
            setPlan(null);
          }}
        />
      </Step>
      <Step n={3} title="What the launcher starts" enabled={planned && !engine}>
        {plan ? (
          <LaunchStep files={plan.files} initial={saved?.execution} onChange={setExecution} />
        ) : null}
      </Step>
      <Step n={4} title="Publisher key" enabled={planned && launchReady && !engine}>
        <KeyStep keys={keys.current} unlocked={keyInfo} onUnlocked={setKeyInfo} />
      </Step>
      <Step n={5} title="Upload" enabled={planned && launchReady && keyInfo !== null}>
        {engine && version ? (
          <UploadStep
            engine={engine}
            version={version}
            onPublished={() =>
              void client.invalidateQueries({ queryKey: versionKeys.list(pkg.id) })
            }
            onNewKey={() => {
              setKeyInfo(null);
              const fresh = deps.workers.key();
              engine.replaceKeys(fresh);
              keys.current = fresh;
              setEngine(null);
            }}
          />
        ) : (
          <button type="button" className="primary" onClick={() => void startUpload()}>
            {saved && Object.keys(saved.confirmed).length > 0 ? "Continue upload" : "Start upload"}
          </button>
        )}
      </Step>
    </div>
  );
}

function CreateVersion({
  packageId,
  onCreated,
}: {
  packageId: string;
  onCreated: (v: Version) => void;
}) {
  const [key] = useState(newIdempotencyKey);
  const [platform, setPlatform] = useState<Platform>("windows-x86_64");
  const [label, setLabel] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (inFlight.current) return;
    const n = [...label.trim()].length;
    if (n < 1 || n > 64) {
      setError("A version label has 1 to 64 characters, for example 1.2.0.");
      return;
    }
    inFlight.current = true;
    setBusy(true);
    setError(null);
    setFailure(null);
    try {
      onCreated(
        (await createVersion(packageId, { platform, version_label: label.trim() }, key)).data,
      );
    } catch (err) {
      if (err instanceof ApiError && err.fieldErrors().version_label)
        setError(`The server says: ${err.fieldErrors().version_label}.`);
      else setFailure(err);
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  return (
    <form onSubmit={(e) => void submit(e)} noValidate className="toolbar">
      <div className="field">
        <label htmlFor="version-platform">Platform</label>
        <select
          id="version-platform"
          value={platform}
          onChange={(e) => setPlatform(e.target.value as Platform)}
        >
          {PLATFORMS.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
      </div>
      <div className="field">
        <label htmlFor="version-label">Version label</label>
        <input
          id="version-label"
          value={label}
          dir="auto"
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? "version-label-error" : undefined}
          onChange={(e) => {
            setLabel(e.target.value);
            setError(null);
          }}
        />
        {error ? (
          <span id="version-label-error" className="field-error">
            {error}
          </span>
        ) : null}
      </div>
      <button type="submit" className="primary" aria-disabled={busy || undefined}>
        {busy ? "Creating…" : "Create version"}
      </button>
      {failure ? <ErrorView error={failure} /> : null}
    </form>
  );
}

function FolderStep({
  packs,
  saved,
  mockPackSize,
  onPlanned,
  onCleared,
}: {
  packs: PackChannel | null;
  saved: SavedUpload | null;
  mockPackSize: number | undefined;
  onPlanned: (folder: PickedFolder, plan: PlanSummary) => void;
  onCleared: () => void;
}) {
  const [folder, setFolder] = useState<PickedFolder | null>(null);
  const [plan, setPlan] = useState<PlanSummary | null>(null);
  const [invalid, setInvalid] = useState<string[]>([]);
  const [changes, setChanges] = useState<string[]>([]);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const use = async (picked: PickedFolder | null) => {
    if (!picked || !packs) return;
    onCleared();
    setPlan(null);
    setInvalid([]);
    setChanges([]);
    setProblem(null);
    setFolder(picked);
    if (picked.files.length === 0) {
      setProblem("This folder has no files.");
      return;
    }
    if (saved) {
      const diff = folderChanges(saved.files, picked.files);
      if (diff.length > 0) {
        setChanges(diff);
        return;
      }
    }
    setBusy(true);
    try {
      const reply = await packs.call({
        kind: "plan",
        files: picked.files,
        directories: picked.directories,
        blobs: picked.blobs,
        ...(mockPackSize ? { mockPackSize } : {}),
      });
      if (reply.kind === "invalid") setInvalid(reply.paths);
      else if (reply.kind === "planned") {
        setPlan(reply.summary);
        onPlanned(picked, reply.summary);
      } else if (reply.kind === "error") setProblem(reply.message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <div className="toolbar">
        <div className="field">
          <label htmlFor="folder-input">Game folder</label>
          <input
            id="folder-input"
            type="file"
            // @ts-expect-error: non-standard attributes, supported by every current browser.
            webkitdirectory=""
            directory=""
            multiple
            onChange={(e) => void use(e.target.files ? fromFileList(e.target.files) : null)}
          />
        </div>
        {canPickDirectory() ? (
          <button type="button" onClick={async () => void use(await pickDirectory())}>
            Choose folder (includes empty folders)
          </button>
        ) : null}
      </div>
      {busy ? <Loading label="Checking the files…" /> : null}
      {problem ? (
        <p className="banner error" role="alert">
          {problem}
        </p>
      ) : null}
      {changes.length > 0 ? (
        <div className="banner error" role="alert">
          <strong>This isn't the folder the upload started with</strong>
          <p>
            {changes.length.toLocaleString("en")} difference{changes.length === 1 ? "" : "s"}. Files
            can't change during an upload; pick the original folder, or abort this version and start
            a new upload.
          </p>
          <VirtualList items={changes} label="Differences" render={(c) => c} height={160} />
        </div>
      ) : null}
      {invalid.length > 0 ? (
        <div className="banner error" role="alert">
          <strong>
            {invalid.length.toLocaleString("en")} path{invalid.length === 1 ? " is" : "s are"} not
            allowed
          </strong>
          <p>
            Rename or remove them, then pick the folder again. Players on Windows, macOS and Linux
            must be able to install every file.
          </p>
          <VirtualList
            items={invalid}
            label="Invalid paths"
            render={(p) => <span className="mono">{p}</span>}
            height={200}
          />
        </div>
      ) : null}
      {plan && folder ? (
        <div>
          <p role="status">
            <strong dir="auto">{folder.name || "Folder"}</strong>:{" "}
            {plan.files.length.toLocaleString("en")} files, {formatSize(plan.totalBytes)},{" "}
            {plan.packs.length} pack{plan.packs.length === 1 ? "" : "s"}
            {folder.directories.length > 0 ? `, ${folder.directories.length} empty folders` : ""}.
          </p>
          <VirtualList
            items={plan.files}
            label="Files to upload"
            render={(f) => (
              <>
                <span className="mono">{f.path}</span>{" "}
                <span className="muted">{formatSize(f.size)}</span>
              </>
            )}
          />
        </div>
      ) : null}
    </div>
  );
}

function LaunchStep({
  files,
  initial,
  onChange,
}: {
  files: PlanSummary["files"];
  initial: Execution | undefined;
  /** `null` while no valid choice is made. */
  onChange: (e: Execution | undefined | null) => void;
}) {
  const first = initial?.launch?.targets[0];
  const [none, setNone] = useState(initial !== undefined && !initial.launch);
  const [exe, setExe] = useState(first?.executable ?? "");
  const [args, setArgs] = useState(first?.args.join(" ") ?? "");
  const candidates = useMemo(
    () => files.filter((f) => LAUNCHABLE.test(f.path.split("/").pop() ?? "")).slice(0, 200),
    [files],
  );
  const known = useMemo(() => new Set(files.map((f) => f.path)), [files]);
  const valid = none || known.has(exe.trim());

  useEffect(() => {
    if (none) onChange(undefined);
    else if (known.has(exe.trim()))
      onChange({
        launch: {
          default: "play",
          targets: [
            {
              id: "play",
              label: "Play",
              executable: exe.trim(),
              args: args.trim() ? args.trim().split(/\s+/) : [],
            },
          ],
        },
      });
    else onChange(null);
  }, [none, exe, args, known, onChange]);

  return (
    <div className="form-grid">
      <div className="field">
        <label htmlFor="launch-exe">Executable (path in the folder)</label>
        <input
          id="launch-exe"
          className="mono"
          list="launch-candidates"
          value={exe}
          disabled={none}
          aria-invalid={!none && exe.trim() !== "" && !valid ? true : undefined}
          aria-describedby="launch-exe-hint"
          onChange={(e) => setExe(e.target.value)}
        />
        <datalist id="launch-candidates">
          {candidates.map((f) => (
            <option key={f.path} value={f.path} />
          ))}
        </datalist>
        <span
          id="launch-exe-hint"
          className={!none && exe.trim() !== "" && !valid ? "field-error" : "muted"}
        >
          {!none && exe.trim() !== "" && !valid
            ? "No file with this path is in the folder."
            : "Start typing to see the programs in the folder."}
        </span>
      </div>
      <div className="field">
        <label htmlFor="launch-args">Arguments (optional, separated by spaces)</label>
        <input
          id="launch-args"
          className="mono"
          value={args}
          disabled={none}
          onChange={(e) => setArgs(e.target.value)}
        />
      </div>
      <div className="field">
        <label>
          <input type="checkbox" checked={none} onChange={(e) => setNone(e.target.checked)} />{" "}
          Nothing to start (tools, add-ons)
        </label>
      </div>
    </div>
  );
}

export function KeyStep({
  keys,
  unlocked,
  onUnlocked,
}: {
  keys: KeyChannel | null;
  unlocked: { keyId: string; label: string } | null;
  onUnlocked: (info: { keyId: string; label: string }) => void;
}) {
  const [file, setFile] = useState<File | null>(null);
  const [passphrase, setPassphrase] = useState("");
  const [error, setError] = useState<{ field: "file" | "passphrase"; text: string } | null>(null);
  const [busy, setBusy] = useState(false);

  if (unlocked)
    return (
      <p role="status" className="banner ok">
        Key ready: {unlocked.label || "publisher key"}{" "}
        <span className="mono">({unlocked.keyId})</span>. It stays inside this page's signing worker
        and is dropped when the upload is finalized.
      </p>
    );

  const unlock = async (e: FormEvent) => {
    e.preventDefault();
    if (!keys || busy) return;
    if (!file) {
      setError({ field: "file", text: "Choose your publisher key file." });
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      const reply = await keys.call({ kind: "unlock", keyfile: bytes, passphrase }, [bytes.buffer]);
      if (reply.kind === "unlocked") {
        setPassphrase("");
        onUnlocked({ keyId: reply.keyId, label: reply.label });
      } else if (reply.kind === "error")
        setError({
          field: reply.code === "wrong_passphrase" ? "passphrase" : "file",
          text: reply.message,
        });
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={(e) => void unlock(e)} noValidate className="toolbar">
      <div className="field">
        <label htmlFor="key-file">Publisher key file</label>
        <input
          id="key-file"
          type="file"
          aria-invalid={error?.field === "file" || undefined}
          aria-describedby={error?.field === "file" ? "key-error" : undefined}
          onChange={(e) => {
            setFile(e.target.files?.[0] ?? null);
            setError(null);
          }}
        />
      </div>
      <div className="field">
        <label htmlFor="key-passphrase">Passphrase</label>
        <input
          id="key-passphrase"
          type="password"
          autoComplete="off"
          value={passphrase}
          aria-invalid={error?.field === "passphrase" || undefined}
          aria-describedby={error?.field === "passphrase" ? "key-error" : undefined}
          onChange={(e) => {
            setPassphrase(e.target.value);
            setError(null);
          }}
        />
      </div>
      <button type="submit" aria-disabled={busy || undefined}>
        {busy ? "Unlocking…" : "Unlock key"}
      </button>
      {error ? (
        <p id="key-error" className="field-error" role="alert">
          {error.text}
        </p>
      ) : null}
    </form>
  );
}

const PHASE_TEXT: Record<Snapshot["phase"]["kind"], string> = {
  ready: "Ready to upload.",
  uploading: "Uploading…",
  paused: "Paused. Your progress is kept, even if you close this page.",
  waiting_network: "The connection dropped. The upload continues by itself when it's back.",
  signing: "Signing the manifest…",
  finalizing: "Uploading the manifest and asking the server to check the version…",
  verifying: "The server is verifying every file…",
  verified: "Verified. The version is ready to publish.",
  failed: "",
};

function UploadStep({
  engine,
  version,
  onPublished,
  onNewKey,
}: {
  engine: UploadEngine;
  version: Version;
  onPublished: () => void;
  onNewKey: () => void;
}) {
  const snap = useSyncExternalStore(engine.subscribe, engine.getSnapshot);
  const [lockLost, setLockLost] = useState(false);
  const release = useRef<Release | null>(null);
  const [published, setPublished] = useState<Version | null>(null);
  const [publishError, setPublishError] = useState<unknown>(null);
  const active = engine.active;

  const run = async () => {
    release.current ??= await acquireUploadLock(version.id);
    if (!release.current) {
      setLockLost(true);
      return;
    }
    setLockLost(false);
    await engine.start();
  };

  // Starts as soon as the step shows; the lock is released when the page goes.
  // biome-ignore lint/correctness/useExhaustiveDependencies: start once per engine.
  useEffect(() => {
    void run();
    return () => {
      engine.pause();
      release.current?.();
      release.current = null;
    };
  }, [engine]);

  // Closing or reloading the tab mid-upload asks first; in-app navigation too.
  useEffect(() => {
    if (!active) return;
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    return () => window.removeEventListener("beforeunload", onBeforeUnload);
  }, [active]);
  const blocker = useBlocker(
    ({ currentLocation, nextLocation }) =>
      active && currentLocation.pathname !== nextLocation.pathname,
  );

  const phase = snap.phase;
  const done = snap.packs.filter((p) => p.status === "done").length;

  return (
    <div>
      {lockLost ? (
        <p className="banner error" role="alert">
          This upload is already running in another tab or window. Continue it there, or close it
          and try again.{" "}
          <button type="button" onClick={() => void run()}>
            Try again
          </button>
        </p>
      ) : null}
      <p role="status">
        {phase.kind === "failed" ? "" : PHASE_TEXT[phase.kind]}
        {phase.kind === "verifying" ? ` ${Math.round(phase.progress * 100)}%` : ""}
      </p>
      <div className="row">
        <progress max={Math.max(1, snap.total)} value={snap.sent} aria-label="Upload progress" />
        <span>
          {formatSize(snap.sent)} of {formatSize(snap.total)} · {done} of {snap.packs.length} packs
        </span>
        {phase.kind === "uploading" || phase.kind === "waiting_network" ? (
          <button type="button" onClick={() => engine.pause()}>
            Pause
          </button>
        ) : null}
        {phase.kind === "paused" ? (
          <button type="button" className="primary" onClick={() => void run()}>
            Resume
          </button>
        ) : null}
      </div>
      {phase.kind === "failed" ? (
        <div className="banner error" role="alert">
          <strong>{phase.title}</strong>
          <p>{phase.text}</p>
          <div className="row">
            {phase.retryable ? (
              <button type="button" onClick={() => void run()}>
                Try again
              </button>
            ) : null}
            {/key|signature|signed/i.test(phase.text) ? (
              <button type="button" onClick={onNewKey}>
                Use another key file
              </button>
            ) : null}
          </div>
        </div>
      ) : null}
      {phase.kind === "verified" ? (
        published ? (
          <p className="banner ok" role="status">
            Published {published.version_label}. Players get it now.{" "}
            <Link to={`/packages/${published.package_id}/versions`}>Back to versions</Link>
          </p>
        ) : (
          <div className="row">
            <button
              type="button"
              className="primary"
              onClick={async () => {
                setPublishError(null);
                try {
                  setPublished((await publishVersion(version.id)).data);
                  onPublished();
                } catch (e) {
                  setPublishError(e);
                }
              }}
            >
              Publish {version.version_label}
            </button>
            <Link to={`/packages/${version.package_id}/versions`}>Publish later</Link>
            {publishError ? <ErrorView error={publishError} /> : null}
          </div>
        )
      ) : null}
      <table>
        <caption className="visually-hidden">Packs</caption>
        <thead>
          <tr>
            <th scope="col">Pack</th>
            <th scope="col">Progress</th>
            <th scope="col">State</th>
          </tr>
        </thead>
        <tbody>
          {snap.packs.map((p) => (
            <tr key={p.index}>
              <td>{p.index + 1}</td>
              <td>
                <progress
                  max={Math.max(1, p.size)}
                  value={p.sent}
                  aria-label={`Pack ${p.index + 1}`}
                />{" "}
                {formatSize(p.sent)} / {formatSize(p.size)}
              </td>
              <td>
                {p.status === "done"
                  ? "Uploaded"
                  : p.status === "uploading"
                    ? "Uploading"
                    : "Waiting"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {blocker.state === "blocked" ? (
        <Modal title="Leave while uploading?" role="alertdialog" onClose={() => blocker.reset()}>
          <p>
            The upload pauses. Come back to this version to continue; you'll pick the folder and
            unlock the key again.
          </p>
          <div className="actions">
            <button type="button" data-autofocus="" onClick={() => blocker.reset()}>
              Stay
            </button>
            <button type="button" className="danger" onClick={() => blocker.proceed()}>
              Pause and leave
            </button>
          </div>
        </Modal>
      ) : null}
    </div>
  );
}
