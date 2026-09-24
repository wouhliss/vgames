import type { ReactNode } from "react";
import { Icon, type IconName } from "../../components/Icon";
import styles from "./Onboarding.module.css";

const TONES = {
  info: { icon: "info", className: "" },
  success: { icon: "check", className: styles.noticeSuccess },
  warning: { icon: "warning", className: styles.noticeWarning },
  danger: { icon: "error", className: styles.noticeDanger },
} satisfies Record<string, { icon: IconName; className: string | undefined }>;

export function Notice({
  tone = "info",
  role,
  children,
}: {
  tone?: keyof typeof TONES;
  role?: "alert" | "status";
  children: ReactNode;
}) {
  const spec = TONES[tone];
  return (
    <div className={`${styles.notice} ${spec.className ?? ""}`} role={role}>
      <Icon name={spec.icon} />
      <div>{children}</div>
    </div>
  );
}

/** "VG1-7K2M-…" shown as large groups of four, for comparing out loud or side by side. */
export function Fingerprint({ value, label }: { value: string; label: string }) {
  const [prefix, ...groups] = value.split("-");
  return (
    <div className={styles.fingerprint}>
      <span className={styles.fingerprintLabel}>{label}</span>
      <p className={styles.groups} data-testid="fingerprint">
        <span className={styles.prefix}>{prefix}</span>
        {groups.map((group, i) => (
          // biome-ignore lint/suspicious/noArrayIndexKey: groups are positional.
          <span key={i}>{group}</span>
        ))}
      </p>
    </div>
  );
}
