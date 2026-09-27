// Select-only combobox (WAI-ARIA APG). A custom listbox instead of a native <select> because native
// popups cannot be driven by the gamepad: focus stays on the trigger and `aria-activedescendant`
// points at the highlighted option, so D-pad intents arrive here as arrow keys.
import {
  type KeyboardEvent,
  type ReactNode,
  useCallback,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import styles from "./Field.module.css";
import { Icon } from "./Icon";
import { popoverContainer } from "./layers";
import { type PopoverPosition, placeBelow, useDismissPopover } from "./popover";
import selectStyles from "./Select.module.css";

export interface SelectOption<V extends string> {
  value: V;
  label: string;
  description?: string;
  disabled?: boolean;
}

export interface SelectProps<V extends string> {
  label: string;
  hideLabel?: boolean;
  value: V | null;
  options: readonly SelectOption<V>[];
  onChange: (value: V) => void;
  placeholder?: string;
  description?: ReactNode;
  error?: string | null;
  disabled?: boolean;
  id?: string;
}

export function Select<V extends string>({
  label,
  hideLabel,
  value,
  options,
  onChange,
  placeholder,
  description,
  error,
  disabled,
  id: idProp,
}: SelectProps<V>) {
  const autoId = useId();
  const id = idProp ?? autoId;
  const listId = `${id}-listbox`;
  const labelId = `${id}-label`;
  const trigger = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1);
  const [position, setPosition] = useState<PopoverPosition | null>(null);
  const typeahead = useRef({ text: "", at: 0 });

  const selectedIndex = options.findIndex((o) => o.value === value);
  const selected = options[selectedIndex];

  const close = useCallback(() => setOpen(false), []);
  useDismissPopover(open, list, trigger, close);

  const openAt = (index: number) => {
    if (disabled) return;
    setActive(index);
    setOpen(true);
  };

  useLayoutEffect(() => {
    if (!open || !trigger.current || !list.current) return;
    const anchor = trigger.current.getBoundingClientRect();
    setPosition(
      placeBelow(
        anchor,
        { width: list.current.offsetWidth, height: list.current.scrollHeight },
        true,
      ),
    );
  }, [open]);

  useLayoutEffect(() => {
    if (!open || active < 0) return;
    document.getElementById(`${id}-opt-${active}`)?.scrollIntoView?.({ block: "nearest" });
  }, [open, active, id]);

  const step = (from: number, delta: 1 | -1): number => {
    for (let i = 1; i <= options.length; i += 1) {
      const next = (from + delta * i + options.length * 2) % options.length;
      if (!options[next]?.disabled) return next;
    }
    return from;
  };
  const edge = (fromEnd: boolean): number => {
    const indexes = options.map((o, i) => (o.disabled ? -1 : i)).filter((i) => i >= 0);
    return (fromEnd ? indexes[indexes.length - 1] : indexes[0]) ?? -1;
  };

  const commit = (index: number) => {
    const option = options[index];
    if (!option || option.disabled) return;
    onChange(option.value);
    setOpen(false);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (e.key.length === 1 && e.key !== " " && !e.ctrlKey && !e.metaKey && !e.altKey) {
      const now = Date.now();
      const text =
        now - typeahead.current.at < 700
          ? typeahead.current.text + e.key.toLowerCase()
          : e.key.toLowerCase();
      typeahead.current = { text, at: now };
      const match = options.findIndex((o) => !o.disabled && o.label.toLowerCase().startsWith(text));
      if (match >= 0) {
        if (open) setActive(match);
        else onChange(options[match]?.value as V);
      }
      e.preventDefault();
      return;
    }
    if (!open) {
      if (["ArrowDown", "ArrowUp", "Enter", " "].includes(e.key)) {
        e.preventDefault();
        openAt(selectedIndex >= 0 ? selectedIndex : e.key === "ArrowUp" ? edge(true) : edge(false));
      }
      return;
    }
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        setActive((a) => step(a, 1));
        return;
      case "ArrowUp":
        e.preventDefault();
        setActive((a) => step(a, -1));
        return;
      case "Home":
      case "PageUp":
        e.preventDefault();
        setActive(edge(false));
        return;
      case "End":
      case "PageDown":
        e.preventDefault();
        setActive(edge(true));
        return;
      case "Enter":
      case " ":
        e.preventDefault();
        commit(active);
        return;
      case "Escape":
        // Closes the list only, not a dialog around it.
        e.preventDefault();
        e.stopPropagation();
        setOpen(false);
        return;
      case "Tab":
        commit(active);
        return;
      case "ArrowLeft":
      case "ArrowRight":
        // Keep the D-pad inside the open list.
        e.preventDefault();
        return;
    }
  };

  return (
    <div className={styles.field}>
      <span id={labelId} className={hideLabel ? "visually-hidden" : styles.label}>
        {label}
      </span>
      {description ? (
        <div id={`${id}-desc`} className={styles.description}>
          {description}
        </div>
      ) : null}
      <button
        ref={trigger}
        id={id}
        type="button"
        role="combobox"
        className={selectStyles.trigger}
        aria-labelledby={`${labelId} ${id}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listId}
        aria-activedescendant={open && active >= 0 ? `${id}-opt-${active}` : undefined}
        aria-invalid={error ? true : undefined}
        aria-describedby={
          [description && `${id}-desc`, error && `${id}-err`].filter(Boolean).join(" ") || undefined
        }
        disabled={disabled}
        onClick={() =>
          open ? setOpen(false) : openAt(selectedIndex >= 0 ? selectedIndex : edge(false))
        }
        onKeyDown={onKeyDown}
        onBlur={(e) => {
          if (!list.current?.contains(e.relatedTarget as Node | null)) setOpen(false);
        }}
      >
        <span className={`${selectStyles.value} ${selected ? "" : selectStyles.placeholder}`}>
          {selected ? selected.label : (placeholder ?? "")}
        </span>
        <Icon name="chevronDown" size={16} />
      </button>
      {error ? (
        <div id={`${id}-err`} className={styles.error}>
          <Icon name="error" size={16} />
          <span>{error}</span>
        </div>
      ) : null}
      {open
        ? createPortal(
            <div
              ref={list}
              id={listId}
              role="listbox"
              data-popover=""
              aria-labelledby={labelId}
              className={selectStyles.listbox}
              style={
                position
                  ? {
                      left: position.left,
                      top: position.top,
                      minWidth: position.minWidth,
                      maxHeight: position.maxHeight,
                    }
                  : { visibility: "hidden" }
              }
            >
              {options.map((option, index) => (
                // biome-ignore lint/a11y/useKeyWithClickEvents: keyboard selection is handled by the combobox (focus never leaves it).
                // biome-ignore lint/a11y/useFocusableInteractive: options are highlighted through aria-activedescendant.
                <div
                  key={option.value}
                  id={`${id}-opt-${index}`}
                  role="option"
                  aria-selected={option.value === value}
                  aria-disabled={option.disabled || undefined}
                  data-active={index === active || undefined}
                  className={selectStyles.option}
                  onPointerMove={() => !option.disabled && setActive(index)}
                  onPointerDown={(e) => e.preventDefault()}
                  onClick={() => commit(index)}
                >
                  <span>
                    {option.label}
                    {option.description ? (
                      <span className={selectStyles.optionDescription}>
                        {" "}
                        — {option.description}
                      </span>
                    ) : null}
                  </span>
                  {option.value === value ? <Icon name="check" size={16} /> : null}
                </div>
              ))}
            </div>,
            popoverContainer(trigger.current),
          )
        : null}
    </div>
  );
}
