// Images (A3-T15): cover, hero, logo and screenshots. Uploads are checked for type (by content, not
// just the name) and size before sending, show progress, and a new cover/hero/logo is set on the
// package right after (If-Match). Deleting asks first. Images that fail to load show a placeholder.
import { useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import { ApiError } from "../../api/errors";
import { deleteAsset, packageKeys, updatePackage } from "../../api/packages";
import type { AdminPackage, Asset } from "../../api/schemas";
import { checkImage, uploadAsset } from "../../api/upload";
import { ErrorView } from "../../app/ErrorView";
import { Modal } from "../../components/Modal";
import { usePackageContext } from "./PackageLayout";

type Slot = "cover" | "hero" | "logo";

const SLOTS: readonly { kind: Slot; label: string; hint: string }[] = [
  { kind: "cover", label: "Cover", hint: "Portrait, shown in the library and the catalog." },
  { kind: "hero", label: "Hero", hint: "Wide banner at the top of the package page." },
  { kind: "logo", label: "Logo", hint: "Transparent PNG or WebP works best." },
];

/** An image from the API, or a placeholder when it can't be loaded. */
export function AssetImage({ asset, alt }: { asset: Asset; alt: string }) {
  const [broken, setBroken] = useState(false);
  if (broken)
    return (
      <div className="image-placeholder" role="img" aria-label={`${alt} (couldn't be loaded)`}>
        Image unavailable
      </div>
    );
  return (
    <img
      className="asset"
      src={asset.url}
      alt={alt}
      width={asset.width}
      height={asset.height}
      loading="lazy"
      onError={() => setBroken(true)}
    />
  );
}

function Uploader({
  pkg,
  kind,
  label,
  onUploaded,
}: {
  pkg: AdminPackage;
  kind: Asset["kind"];
  label: string;
  onUploaded: (asset: Asset) => Promise<void>;
}) {
  const inputId = useId();
  const [file, setFile] = useState<File | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [progress, setProgress] = useState<number | null>(null);
  const [error, setError] = useState<unknown>(null);

  const upload = async () => {
    if (!file || progress !== null) return;
    setError(null);
    const why = await checkImage(file);
    setProblem(why);
    if (why) return;
    setProgress(0);
    try {
      const asset = await uploadAsset(pkg.id, kind, file, setProgress);
      await onUploaded(asset);
      setFile(null);
    } catch (e) {
      setError(e);
    } finally {
      setProgress(null);
    }
  };

  return (
    <div className="toolbar">
      <div className="field">
        <label htmlFor={inputId}>{label}</label>
        <input
          id={inputId}
          type="file"
          accept="image/jpeg,image/png,image/webp"
          aria-invalid={problem ? true : undefined}
          aria-describedby={problem ? `${inputId}-problem` : undefined}
          onChange={async (e) => {
            const next = e.target.files?.[0] ?? null;
            setFile(next);
            setError(null);
            setProblem(next ? await checkImage(next) : null);
          }}
        />
        {problem ? (
          <span id={`${inputId}-problem`} className="field-error">
            {problem}
          </span>
        ) : null}
      </div>
      <button
        type="button"
        disabled={!file || problem !== null || progress !== null}
        onClick={() => void upload()}
      >
        Upload
      </button>
      {progress !== null ? (
        <progress max={1} value={progress} aria-label={`Uploading ${file?.name ?? "image"}`}>
          {Math.round(progress * 100)}%
        </progress>
      ) : null}
      {error ? <ErrorView error={error} /> : null}
    </div>
  );
}

export function ImagesTab() {
  const { response } = usePackageContext();
  const pkg = response.data;
  const client = useQueryClient();
  const [deleting, setDeleting] = useState<{ asset: Asset; label: string } | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);

  const reload = () => client.invalidateQueries({ queryKey: packageKeys.one(pkg.id) });

  /** Points the package's cover/hero/logo at a new image. */
  const setSlot = async (kind: Slot, asset: Asset) => {
    try {
      const result = await updatePackage(pkg.id, response.etag ?? "", {
        [`${kind}_asset_id`]: asset.id,
      });
      client.setQueryData(packageKeys.one(pkg.id), result);
      setMessage(`New ${kind} uploaded and in use.`);
    } catch (e) {
      if (e instanceof ApiError && e.status === 412) {
        setMessage(
          `The image was uploaded, but the package changed meanwhile. Reloaded it; upload the ${kind} again to use it.`,
        );
        await reload();
      } else setFailure(e);
    }
  };

  const confirmDelete = async () => {
    if (!deleting) return;
    try {
      await deleteAsset(deleting.asset.id);
      setMessage(`${deleting.label} deleted.`);
      await reload();
    } catch (e) {
      setFailure(e);
    } finally {
      setDeleting(null);
    }
  };

  return (
    <div>
      {message ? (
        <p role="status" className="banner ok">
          {message}
        </p>
      ) : null}
      {failure ? <ErrorView error={failure} /> : null}
      {SLOTS.map(({ kind, label, hint }) => {
        const asset = pkg[kind];
        return (
          <section key={kind} aria-labelledby={`slot-${kind}`}>
            <h2 id={`slot-${kind}`}>{label}</h2>
            <p className="muted">{hint}</p>
            {asset ? (
              <div className="row">
                <AssetImage asset={asset} alt={`${label} of ${pkg.title}`} />
                <button
                  type="button"
                  className="danger"
                  onClick={() => setDeleting({ asset, label })}
                >
                  Delete {label.toLowerCase()}…
                </button>
              </div>
            ) : (
              <p>No {label.toLowerCase()} yet.</p>
            )}
            <Uploader
              pkg={pkg}
              kind={kind}
              label={`Upload a new ${label.toLowerCase()}`}
              onUploaded={(a) => setSlot(kind, a)}
            />
          </section>
        );
      })}
      <section aria-labelledby="slot-screenshots">
        <h2 id="slot-screenshots">Screenshots</h2>
        {pkg.screenshots?.length ? (
          <ul className="screenshots">
            {pkg.screenshots.map((shot, i) => (
              <li key={shot.id}>
                <AssetImage asset={shot} alt={`Screenshot ${i + 1} of ${pkg.title}`} />
                <button
                  type="button"
                  className="danger"
                  onClick={() => setDeleting({ asset: shot, label: `Screenshot ${i + 1}` })}
                >
                  Delete screenshot {i + 1}…
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <p>No screenshots yet.</p>
        )}
        <Uploader
          pkg={pkg}
          kind="screenshot"
          label="Upload a screenshot"
          onUploaded={async () => {
            setMessage("Screenshot uploaded.");
            await reload();
          }}
        />
      </section>
      {deleting ? (
        <Modal
          title={`Delete ${deleting.label.toLowerCase()}?`}
          role="alertdialog"
          onClose={() => setDeleting(null)}
        >
          <p>The image is removed from the package and from storage. Players see it disappear.</p>
          <div className="actions">
            <button type="button" data-autofocus="" onClick={() => setDeleting(null)}>
              Cancel
            </button>
            <button type="button" className="danger" onClick={() => void confirmDelete()}>
              Delete
            </button>
          </div>
        </Modal>
      ) : null}
    </div>
  );
}
