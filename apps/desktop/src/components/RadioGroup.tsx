// Single choice from a short list of rich options (WAI-ARIA APG radio group, roving tabindex).
// Arrow keys (or the D-pad) move and select; at the first or last option they are left alone so the
// spatial navigation can leave the group, which a controller user needs to reach the dialog buttons.
import { type KeyboardEvent, type ReactNode, useId, useRef } from "react";
import { focusElement } from "../nav/focus";
import styles from "./RadioGroup.module.css";

export interface RadioOption<V extends string> {
  value: V;
  label: ReactNode;
  description?: ReactNode;
  /** Extra content on the trailing side (sizes, badges). */
  aside?: ReactNode;
  disabled?: boolean;
}

export interface RadioGroupProps<V extends string> {
  label: string;
  hideLabel?: boolean;
  value: V | null;
  options: readonly RadioOption<V>[];
  onChange: (value: V) => void;
}

export function RadioGroup<V extends string>({
  label,
  hideLabel,
  value,
  options,
  onChange,
}: RadioGroupProps<V>) {
  const id = useId();
  const listRef = useRef<HTMLDivElement>(null);
  const enabled = options.filter((o) => !o.disabled);
  const focusable = options.some((o) => o.value === value && !o.disabled)
    ? value
    : (enabled[0]?.value ?? null);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const forward = e.key === "ArrowDown" || e.key === "ArrowRight";
    const backward = e.key === "ArrowUp" || e.key === "ArrowLeft";
    if (!forward && !backward) return;
    const radios = Array.from(
      listRef.current?.querySelectorAll<HTMLElement>(
        "[role='radio']:not([aria-disabled='true'])",
      ) ?? [],
    );
    const index = radios.indexOf(document.activeElement as HTMLElement);
    const next = radios[index + (forward ? 1 : -1)];
    // At an edge the key falls through to spatial navigation.
    if (index < 0 || !next) return;
    e.preventDefault();
    focusElement(next);
    const option = enabled.find((o) => o.value === next.dataset.value);
    if (option) onChange(option.value);
  };

  return (
    <div className={styles.group}>
      <div id={`${id}-label`} className={hideLabel ? "visually-hidden" : styles.label}>
        {label}
      </div>
      <div
        ref={listRef}
        role="radiogroup"
        aria-labelledby={`${id}-label`}
        className={styles.list}
        onKeyDown={onKeyDown}
      >
        {options.map((option) => {
          const checked = option.value === value;
          return (
            // biome-ignore lint/a11y/useSemanticElements: native radios move the selection on arrow keys before the spatial navigation sees them; this is the APG radio pattern on buttons.
            <button
              key={option.value}
              type="button"
              role="radio"
              data-value={option.value}
              aria-checked={checked}
              aria-disabled={option.disabled || undefined}
              aria-labelledby={`${id}-${option.value}-label`}
              aria-describedby={
                [
                  option.description ? `${id}-${option.value}-desc` : "",
                  option.aside ? `${id}-${option.value}-aside` : "",
                ]
                  .filter(Boolean)
                  .join(" ") || undefined
              }
              tabIndex={option.value === focusable ? 0 : -1}
              className={styles.option}
              onClick={() => {
                if (!option.disabled) onChange(option.value);
              }}
            >
              <span className={styles.dot} aria-hidden="true" />
              <span className={styles.text}>
                <span id={`${id}-${option.value}-label`} className={styles.optionLabel}>
                  {option.label}
                </span>
                {option.description ? (
                  <span id={`${id}-${option.value}-desc`} className={styles.description}>
                    {option.description}
                  </span>
                ) : null}
              </span>
              {option.aside ? (
                <span id={`${id}-${option.value}-aside`} className={styles.aside}>
                  {option.aside}
                </span>
              ) : null}
            </button>
          );
        })}
      </div>
    </div>
  );
}
