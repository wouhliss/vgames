import { type InputHTMLAttributes, type ReactNode, useId } from "react";
import styles from "./Field.module.css";

export interface CheckboxProps
  extends Omit<InputHTMLAttributes<HTMLInputElement>, "type" | "onChange"> {
  label: ReactNode;
  description?: ReactNode;
  onCheckedChange?: (checked: boolean) => void;
}

export function Checkbox({
  label,
  description,
  onCheckedChange,
  id: idProp,
  ...input
}: CheckboxProps) {
  const autoId = useId();
  const id = idProp ?? autoId;
  return (
    <label className={styles.check} htmlFor={id}>
      <input
        {...input}
        id={id}
        type="checkbox"
        className={styles.checkbox}
        aria-describedby={description ? `${id}-desc` : undefined}
        onChange={(e) => onCheckedChange?.(e.target.checked)}
      />
      <span className={styles.checkText}>
        <span>{label}</span>
        {description ? (
          <span id={`${id}-desc`} className={styles.description}>
            {description}
          </span>
        ) : null}
      </span>
    </label>
  );
}
