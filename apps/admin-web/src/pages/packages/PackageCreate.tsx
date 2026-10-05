// Create a package: title, optional slug (with a live preview of the one the server derives), optional
// Steam/IGDB ids. One Idempotency-Key per form instance, so a double submit or a retry after a lost
// response creates exactly one package. Then the editor opens.
import { useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useEffect, useRef, useState } from "react";
import { Link, useNavigate } from "react-router";
import { z } from "zod";
import { ApiError } from "../../api/errors";
import { newIdempotencyKey } from "../../api/http";
import { createPackage } from "../../api/packages";
import { RESTORED_MESSAGE, useDraft } from "../../app/drafts";
import { ErrorView } from "../../app/ErrorView";
import { Field } from "../../components/Field";
import { FIELDS, length, parseId, SLUG, slugify, validateField } from "./model";

const spec = (name: "title" | "slug" | "steam_app_id" | "igdb_id") => {
  const found = FIELDS.find((f) => f.name === name);
  if (!found) throw new Error(name);
  return found;
};

const CreateDraft = z.object({
  title: z.string(),
  slug: z.string(),
  steam: z.string(),
  igdb: z.string(),
  fetchMetadata: z.boolean(),
});
type CreateDraft = z.infer<typeof CreateDraft>;

export function PackageCreate() {
  const navigate = useNavigate();
  const client = useQueryClient();
  const [key] = useState(newIdempotencyKey);
  const restored = useDraft("package-create", CreateDraft, (): CreateDraft | null =>
    title || slug || steam || igdb ? { title, slug, steam, igdb, fetchMetadata } : null,
  );
  const [title, setTitle] = useState(restored?.title ?? "");
  const [slug, setSlug] = useState(restored?.slug ?? "");
  const [steam, setSteam] = useState(restored?.steam ?? "");
  const [igdb, setIgdb] = useState(restored?.igdb ?? "");
  const [fetchMetadata, setFetchMetadata] = useState(restored?.fetchMetadata ?? true);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  // Synchronous guard: a double click can fire twice before React re-renders.
  const inFlight = useRef(false);
  const titleRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    titleRef.current?.focus();
  }, []);

  const preview = slug.trim() || slugify(title);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (inFlight.current) return;
    const local: Record<string, string> = {};
    const titleError = validateField(spec("title"), title);
    if (titleError) local.title = titleError;
    if (slug.trim() && !SLUG.test(slug.trim())) {
      const slugError = validateField(spec("slug"), slug);
      if (slugError) local.slug = slugError;
    }
    if (parseId(steam) === undefined) local.steam_app_id = "Must be a whole number of at least 1.";
    if (parseId(igdb) === undefined) local.igdb_id = "Must be a whole number of at least 1.";
    setErrors(local);
    setFailure(null);
    if (Object.keys(local).length > 0) return;
    inFlight.current = true;
    setBusy(true);
    let created = false;
    try {
      const steamId = parseId(steam);
      const igdbId = parseId(igdb);
      const { data } = await createPackage(
        {
          title: title.trim(),
          ...(slug.trim() ? { slug: slug.trim() } : {}),
          ...(steamId ? { steam_app_id: steamId } : {}),
          ...(igdbId ? { igdb_id: igdbId } : {}),
          fetch_metadata: fetchMetadata,
        },
        key,
      );
      // Created: the guard stays set, so a click before the editor opens can't submit again.
      created = true;
      await client.invalidateQueries({ queryKey: ["admin-packages"] });
      navigate(`/packages/${data.id}`, { replace: true });
    } catch (error) {
      if (
        error instanceof ApiError &&
        error.status === 400 &&
        Object.keys(error.fieldErrors()).length > 0
      ) {
        setErrors(error.fieldErrors());
      } else if (error instanceof ApiError && error.code === "slug_taken") {
        setErrors({ slug: "Another package already uses this slug. Pick a different one." });
      } else {
        setFailure(error);
      }
    } finally {
      if (!created) {
        inFlight.current = false;
        setBusy(false);
      }
    }
  };

  return (
    <section aria-labelledby="page-title" className="form-grid">
      <p>
        <Link to="/packages">← Packages</Link>
      </p>
      <h1 id="page-title">Create package</h1>
      {restored ? (
        <p role="status" className="muted">
          {RESTORED_MESSAGE}
        </p>
      ) : null}
      {failure ? <ErrorView error={failure} /> : null}
      <form onSubmit={(e) => void submit(e)} noValidate>
        <Field
          label="Title (required)"
          error={errors.title}
          count={{ value: length(title.trim()), max: 200 }}
        >
          {(p) => (
            <input
              {...p}
              dir="auto"
              value={title}
              required
              ref={titleRef}
              onChange={(e) => setTitle(e.target.value)}
            />
          )}
        </Field>
        <Field
          label="Slug (optional)"
          hint={`Leave empty to use "${preview}". Lowercase letters, digits and dashes.`}
          error={errors.slug}
        >
          {(p) => (
            <input
              {...p}
              className="mono"
              value={slug}
              spellCheck={false}
              autoCapitalize="none"
              onChange={(e) => setSlug(e.target.value)}
            />
          )}
        </Field>
        <Field label="Steam app id (optional)" error={errors.steam_app_id}>
          {(p) => (
            <input
              {...p}
              inputMode="numeric"
              value={steam}
              onChange={(e) => setSteam(e.target.value)}
            />
          )}
        </Field>
        <Field label="IGDB id (optional)" error={errors.igdb_id}>
          {(p) => (
            <input
              {...p}
              inputMode="numeric"
              value={igdb}
              onChange={(e) => setIgdb(e.target.value)}
            />
          )}
        </Field>
        <div className="field">
          <label>
            <input
              type="checkbox"
              checked={fetchMetadata}
              onChange={(e) => setFetchMetadata(e.target.checked)}
            />{" "}
            Look up metadata from IGDB and Steam after creating
          </label>
        </div>
        <div className="row">
          <button type="submit" className="primary" aria-disabled={busy || undefined}>
            {busy ? "Creating…" : "Create package"}
          </button>
          <Link to="/packages">Cancel</Link>
        </div>
      </form>
    </section>
  );
}
