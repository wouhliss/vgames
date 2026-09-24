// Tabs (WAI-ARIA APG, automatic activation). Left/Right/Home/End move between tabs; LB/RB on a
// controller (or Ctrl+PageUp/PageDown) switch tabs from anywhere inside the tab set.
import { type KeyboardEvent, type ReactNode, useEffect, useId, useRef } from "react";
import { focusElement } from "../nav/focus";
import { useOptionalNav } from "../nav/NavProvider";
import { Glyph } from "./Glyph";
import styles from "./Tabs.module.css";

export interface TabItem<V extends string> {
  value: V;
  label: ReactNode;
}

export interface TabsProps<V extends string> {
  /** Accessible name of the tab list. */
  label: string;
  items: readonly TabItem<V>[];
  value: V;
  onChange: (value: V) => void;
  children: ReactNode;
}

export function Tabs<V extends string>({ label, items, value, onChange, children }: TabsProps<V>) {
  const id = useId();
  const listRef = useRef<HTMLDivElement>(null);
  const registerTabs = useOptionalNav()?.registerTabs;
  const index = Math.max(
    0,
    items.findIndex((i) => i.value === value),
  );

  const select = (next: number, focus: boolean) => {
    const count = items.length;
    if (count === 0) return;
    const wrapped = (next + count) % count;
    const item = items[wrapped];
    if (!item) return;
    onChange(item.value);
    if (focus) {
      const tab = listRef.current?.querySelectorAll<HTMLElement>("[role='tab']")[wrapped];
      if (tab) focusElement(tab);
    }
  };
  const selectRef = useRef(select);
  selectRef.current = select;
  const indexRef = useRef(index);
  indexRef.current = index;

  useEffect(() => {
    const el = listRef.current;
    if (!registerTabs || !el) return;
    return registerTabs(el, (dir) => {
      const focusInList = el.contains(document.activeElement);
      selectRef.current(indexRef.current + dir, focusInList);
    });
  }, [registerTabs]);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const keys: Record<string, number> = {
      ArrowRight: index + 1,
      ArrowLeft: index - 1,
      Home: 0,
      End: items.length - 1,
    };
    const next = keys[e.key];
    if (next === undefined) return;
    e.preventDefault();
    select(next, true);
  };

  return (
    <div className={styles.tabs}>
      <div className={styles.listRow}>
        <Glyph action="tab_prev" className={styles.glyph} />
        <div
          ref={listRef}
          role="tablist"
          aria-label={label}
          className={styles.list}
          onKeyDown={onKeyDown}
        >
          {items.map((item, i) => {
            const selected = i === index;
            return (
              <button
                key={item.value}
                id={`${id}-tab-${item.value}`}
                type="button"
                role="tab"
                aria-selected={selected}
                aria-controls={`${id}-panel`}
                tabIndex={selected ? 0 : -1}
                className={styles.tab}
                onClick={() => select(i, false)}
              >
                {item.label}
              </button>
            );
          })}
        </div>
        <Glyph action="tab_next" className={styles.glyph} />
      </div>
      <div
        id={`${id}-panel`}
        role="tabpanel"
        aria-labelledby={`${id}-tab-${items[index]?.value ?? ""}`}
        className={styles.panel}
      >
        {children}
      </div>
    </div>
  );
}
