import type { ButtonHTMLAttributes, MouseEvent, ReactNode, Ref } from "react";
import { t } from "../i18n";
import styles from "./Button.module.css";
import { Icon, type IconName } from "./Icon";
import { Tooltip } from "./Tooltip";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";
export type ButtonSize = "sm" | "md" | "lg";

export interface ButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "type"> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  /** Shows a spinner and ignores clicks, but keeps focus (a disabled button would drop it). */
  loading?: boolean;
  icon?: IconName;
  block?: boolean;
  type?: "button" | "submit" | "reset";
  ref?: Ref<HTMLButtonElement>;
}

function cx(...names: (string | false | undefined)[]): string {
  return names.filter(Boolean).join(" ");
}

export function Button({
  variant = "secondary",
  size = "md",
  loading = false,
  icon,
  block = false,
  type = "button",
  className,
  children,
  onClick,
  ...rest
}: ButtonProps) {
  const inert = loading || rest["aria-disabled"] === true || rest["aria-disabled"] === "true";
  return (
    <button
      {...rest}
      type={type}
      className={cx(
        styles.button,
        styles[variant],
        size !== "md" && styles[size],
        block && styles.block,
        className,
      )}
      aria-busy={loading || undefined}
      aria-disabled={inert || undefined}
      onClick={(e: MouseEvent<HTMLButtonElement>) => {
        if (inert) {
          e.preventDefault();
          return;
        }
        onClick?.(e);
      }}
    >
      {loading ? (
        <span className={styles.spinner} aria-hidden="true" />
      ) : icon ? (
        <Icon name={icon} size={18} />
      ) : null}
      {children}
      {loading ? <span className="visually-hidden">{t("common.loading")}</span> : null}
    </button>
  );
}

export interface IconButtonProps extends Omit<ButtonProps, "children" | "icon" | "block"> {
  icon: IconName;
  /** Accessible name, also shown as a tooltip. Required: an icon alone has no name. */
  label: string;
  /** Extra visible content next to the icon, e.g. a count badge. */
  adornment?: ReactNode;
  tooltip?: boolean;
}

export function IconButton({
  icon,
  label,
  adornment,
  tooltip = true,
  variant = "ghost",
  size = "md",
  className,
  ...rest
}: IconButtonProps) {
  const button = (
    <Button
      {...rest}
      variant={variant}
      size={size}
      className={cx(styles.iconButton, className)}
      aria-label={label}
    >
      <Icon name={icon} size={size === "sm" ? 16 : 20} />
      {adornment}
    </Button>
  );
  return tooltip ? <Tooltip content={label}>{button}</Tooltip> : button;
}
