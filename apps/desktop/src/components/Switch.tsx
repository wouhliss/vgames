import { type ReactNode, useId } from "react";
import styles from "./Field.module.css";

export interface SwitchProps {
  label: ReactNode;
  description?: ReactNode;
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  disabled?: boolean;
  id?: string;
}

export function Switch({
  label,
  description,
  checked,
  onCheckedChange,
  disabled,
  id: idProp,
}: SwitchProps) {
  const autoId = useId();
  const id = idProp ?? autoId;
  return (
    <div className={styles.switchRow}>
      <span className={styles.checkText}>
        <label htmlFor={id}>{label}</label>
        {description ? (
          <span id={`${id}-desc`} className={styles.description}>
            {description}
          </span>
        ) : null}
      </span>
      <button
        id={id}
        type="button"
        role="switch"
        aria-checked={checked}
        aria-describedby={description ? `${id}-desc` : undefined}
        disabled={disabled}
        className={styles.switch}
        onClick={() => onCheckedChange(!checked)}
      />
    </div>
  );
}
