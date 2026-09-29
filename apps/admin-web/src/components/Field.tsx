// A labelled form field with its hint, error and (optionally) a character counter, all wired to
// the control through aria-describedby.
import { type ReactNode, useId } from "react";

export function Field({
  label,
  hint,
  error,
  count,
  badge,
  children,
}: {
  label: string;
  hint?: string | undefined;
  error?: string | null | undefined;
  /** Shown as "n / max" under the control. */
  count?: { value: number; max: number } | undefined;
  badge?: ReactNode;
  children: (props: {
    id: string;
    "aria-describedby": string | undefined;
    "aria-invalid": true | undefined;
  }) => ReactNode;
}) {
  const id = useId();
  const described = [hint && `${id}-hint`, error && `${id}-error`, count && `${id}-count`]
    .filter(Boolean)
    .join(" ");
  return (
    <div className="field">
      <div className="row">
        <label htmlFor={id}>{label}</label>
        {badge}
      </div>
      {children({
        id,
        "aria-describedby": described || undefined,
        "aria-invalid": error ? true : undefined,
      })}
      {hint ? (
        <span id={`${id}-hint`} className="muted">
          {hint}
        </span>
      ) : null}
      {count ? (
        <span id={`${id}-count`} className={count.value > count.max ? "field-error" : "muted"}>
          {count.value.toLocaleString("en")} / {count.max.toLocaleString("en")}
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
