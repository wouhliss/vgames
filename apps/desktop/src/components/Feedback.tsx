// Presentational building blocks: Card, Badge, EmptyState, ErrorState, LoadingState.
import { type ElementType, type ReactNode, useState } from "react";
import { t } from "../i18n";
import { Button } from "./Button";
import styles from "./Feedback.module.css";
import { Icon, type IconName } from "./Icon";

export function Card({
  as: Tag = "div",
  raised = false,
  className,
  children,
  ...rest
}: {
  as?: ElementType | undefined;
  raised?: boolean | undefined;
  className?: string | undefined;
  children: ReactNode;
  "aria-label"?: string | undefined;
  "aria-labelledby"?: string | undefined;
}) {
  return (
    <Tag
      {...rest}
      className={[styles.card, raised && styles.raised, className].filter(Boolean).join(" ")}
    >
      {children}
    </Tag>
  );
}

export type BadgeTone = "neutral" | "accent" | "info" | "success" | "warning" | "danger";

export function Badge({
  tone = "neutral",
  icon,
  children,
}: {
  tone?: BadgeTone;
  icon?: IconName;
  children: ReactNode;
}) {
  return (
    <span className={`${styles.badge} ${styles[tone]}`}>
      {icon ? <Icon name={icon} size={12} /> : null}
      <span className={styles.badgeText}>{children}</span>
    </span>
  );
}

export function EmptyState({
  icon = "info",
  title,
  description,
  action,
}: {
  icon?: IconName;
  title: string;
  description?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className={styles.state}>
      <span className={styles.stateIcon}>
        <Icon name={icon} size={40} />
      </span>
      <h2>{title}</h2>
      {description ? <p className={styles.stateText}>{description}</p> : null}
      {action ? <div className={styles.stateActions}>{action}</div> : null}
    </div>
  );
}

export function ErrorState({
  title,
  description,
  onRetry,
  details,
  action,
}: {
  title: string;
  description?: ReactNode;
  onRetry?: () => void;
  /** Technical details the user can copy into a bug report. Never include secrets. */
  details?: string;
  action?: ReactNode;
}) {
  const [copied, setCopied] = useState(false);
  return (
    <div className={styles.state} role="alert">
      <span className={styles.errorIcon}>
        <Icon name="error" size={40} />
      </span>
      <h2>{title}</h2>
      {description ? <p className={styles.stateText}>{description}</p> : null}
      <div className={styles.stateActions}>
        {onRetry ? (
          <Button variant="primary" icon="refresh" onClick={onRetry}>
            {t("common.retry")}
          </Button>
        ) : null}
        {action}
        {details ? (
          <Button
            icon="copy"
            onClick={() => {
              void copyText(details).then((ok) => setCopied(ok));
            }}
          >
            {copied ? t("common.copied") : t("error.copyDetails")}
          </Button>
        ) : null}
      </div>
    </div>
  );
}

export function LoadingState({ label }: { label?: string }) {
  return (
    <div className={styles.loading} role="status">
      <span className={styles.spinner} aria-hidden="true" />
      <span>{label ?? t("common.loading")}</span>
    </div>
  );
}

export function Spinner({ label }: { label: string }) {
  return <span className={styles.spinner} role="status" aria-label={label} />;
}

/** Clipboard write that never throws (the WebView may refuse without a user gesture). */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}
