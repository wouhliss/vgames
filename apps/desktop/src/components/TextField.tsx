import {
  type InputHTMLAttributes,
  type ReactNode,
  type Ref,
  type TextareaHTMLAttributes,
  useId,
} from "react";
import { formatNumber, t } from "../i18n";
import styles from "./Field.module.css";
import { Icon } from "./Icon";

interface FieldChrome {
  label: string;
  /** Hide the label visually (it stays the accessible name). */
  hideLabel?: boolean | undefined;
  description?: ReactNode | undefined;
  error?: string | null | undefined;
  required?: boolean | undefined;
}

function describedBy(...ids: unknown[]): string | undefined {
  const joined = ids.filter((v): v is string => typeof v === "string" && v !== "").join(" ");
  return joined === "" ? undefined : joined;
}

function FieldFrame({
  id,
  label,
  hideLabel,
  description,
  error,
  required,
  size,
  children,
  footer,
}: FieldChrome & {
  id: string;
  size?: "md" | "lg" | undefined;
  children: ReactNode;
  footer?: ReactNode | undefined;
}) {
  return (
    <div className={`${styles.field} ${size === "lg" ? styles.lg : ""}`}>
      <label htmlFor={id} className={hideLabel ? "visually-hidden" : styles.label}>
        {label}
        {required ? <span className={styles.required}> {t("common.required")}</span> : null}
      </label>
      {description ? (
        <div id={`${id}-desc`} className={styles.description}>
          {description}
        </div>
      ) : null}
      {children}
      {error ? (
        <div id={`${id}-err`} className={styles.error}>
          <Icon name="error" size={16} />
          <span>{error}</span>
        </div>
      ) : null}
      {footer}
    </div>
  );
}

export interface TextFieldProps
  extends FieldChrome,
    Omit<InputHTMLAttributes<HTMLInputElement>, "size" | "children" | "prefix"> {
  size?: "md" | "lg";
  prefix?: ReactNode;
  suffix?: ReactNode;
  ref?: Ref<HTMLInputElement>;
}

export function TextField({
  label,
  hideLabel,
  description,
  error,
  required,
  size = "md",
  prefix,
  suffix,
  id: idProp,
  ...input
}: TextFieldProps) {
  const autoId = useId();
  const id = idProp ?? autoId;
  return (
    <FieldFrame {...{ id, label, hideLabel, description, error, required, size }}>
      <div className={styles.control} data-invalid={error ? "true" : undefined}>
        {prefix}
        <input
          {...input}
          id={id}
          className={styles.input}
          required={required}
          aria-invalid={error ? true : undefined}
          aria-describedby={describedBy(
            description && `${id}-desc`,
            error && `${id}-err`,
            input["aria-describedby"],
          )}
        />
        {suffix}
      </div>
    </FieldFrame>
  );
}

export interface TextAreaProps
  extends FieldChrome,
    Omit<TextareaHTMLAttributes<HTMLTextAreaElement>, "children"> {
  /** Shows a live character counter and marks the field invalid past the limit. */
  maxChars?: number;
  ref?: Ref<HTMLTextAreaElement>;
}

export function TextArea({
  label,
  hideLabel,
  description,
  error,
  required,
  maxChars,
  id: idProp,
  ...area
}: TextAreaProps) {
  const autoId = useId();
  const id = idProp ?? autoId;
  const length = typeof area.value === "string" ? [...area.value].length : 0;
  const over = maxChars !== undefined && length > maxChars;
  const counter =
    maxChars !== undefined ? (
      <div
        id={`${id}-count`}
        className={styles.counter}
        data-over={over ? "true" : undefined}
        aria-live="polite"
      >
        {t("field.characterCount", { count: length, max: formatNumber(maxChars) })}
      </div>
    ) : null;
  return (
    <FieldFrame {...{ id, label, hideLabel, description, error, required }} footer={counter}>
      <div className={styles.control} data-invalid={error || over ? "true" : undefined}>
        <textarea
          {...area}
          id={id}
          className={styles.textarea}
          required={required}
          aria-invalid={error || over ? true : undefined}
          aria-describedby={describedBy(
            description && `${id}-desc`,
            error && `${id}-err`,
            maxChars !== undefined && `${id}-count`,
          )}
        />
      </div>
    </FieldFrame>
  );
}
