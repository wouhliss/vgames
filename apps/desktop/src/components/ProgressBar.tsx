import { formatPercent } from "../i18n";
import styles from "./ProgressBar.module.css";

export interface ProgressBarProps {
  /** Accessible name; shown above the bar unless `hideLabel`. */
  label: string;
  hideLabel?: boolean;
  /** Fraction 0–1. `null`/`undefined` renders the indeterminate state. */
  value?: number | null;
  /** Human text for assistive technology and the visible value, e.g. "1.2 GB of 4 GB". */
  valueText?: string;
  tone?: "accent" | "success" | "warning" | "danger";
  size?: "sm" | "md";
}

export function ProgressBar({
  label,
  hideLabel,
  value,
  valueText,
  tone = "accent",
  size = "md",
}: ProgressBarProps) {
  const determinate = typeof value === "number" && Number.isFinite(value);
  const fraction = determinate ? Math.min(1, Math.max(0, value)) : 0;
  const percent = Math.round(fraction * 100);
  const shown = valueText ?? (determinate ? formatPercent(fraction) : undefined);
  return (
    <div
      className={[
        styles.wrapper,
        size === "sm" && styles.sm,
        !determinate && styles.indeterminate,
        tone !== "accent" && styles[tone],
      ]
        .filter(Boolean)
        .join(" ")}
    >
      {hideLabel ? null : (
        <div className={styles.header} aria-hidden="true">
          <span className={styles.label}>{label}</span>
          {shown ? <span className={styles.valueText}>{shown}</span> : null}
        </div>
      )}
      <div
        role="progressbar"
        aria-label={label}
        aria-valuemin={determinate ? 0 : undefined}
        aria-valuemax={determinate ? 100 : undefined}
        aria-valuenow={determinate ? percent : undefined}
        aria-valuetext={determinate ? shown : undefined}
        className={styles.track}
      >
        <div className={styles.fill} style={determinate ? { width: `${percent}%` } : undefined} />
      </div>
    </div>
  );
}
