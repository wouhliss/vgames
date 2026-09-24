// Toast notifications. Polite toasts (info/success) and assertive ones (warning/danger) go into two
// persistent live regions, so screen readers announce them once. Auto-dismiss pauses while the
// pointer or focus is inside the region; toasts with an action stay at least 10 s.
import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { t } from "../i18n";
import { Button, IconButton } from "./Button";
import { Icon, type IconName } from "./Icon";
import styles from "./Toast.module.css";

export type ToastTone = "info" | "success" | "warning" | "danger";

export interface ToastInput {
  title: string;
  description?: string;
  tone?: ToastTone;
  action?: { label: string; onClick: () => void };
  /** Milliseconds before auto-dismiss; `null` keeps it until dismissed. */
  duration?: number | null;
}

interface ToastItem extends ToastInput {
  id: number;
  tone: ToastTone;
}

interface ToastApi {
  toast: (input: ToastInput) => number;
  dismiss: (id: number) => void;
}

const ToastContext = createContext<ToastApi | null>(null);

export function useToast(): ToastApi {
  const ctx = useContext(ToastContext);
  if (!ctx) throw new Error("useToast must be used inside <ToastProvider>");
  return ctx;
}

const ICONS: Record<ToastTone, IconName> = {
  info: "info",
  success: "check",
  warning: "warning",
  danger: "error",
};
const MAX_VISIBLE = 4;
const DEFAULT_DURATION = 6000;
const ACTION_MIN_DURATION = 10_000;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<ToastItem[]>([]);
  const nextId = useRef(1);
  const timers = useRef(
    new Map<
      number,
      { remaining: number; started: number; handle?: ReturnType<typeof setTimeout> }
    >(),
  );
  const paused = useRef(false);

  const dismiss = useCallback((id: number) => {
    const timer = timers.current.get(id);
    if (timer?.handle) clearTimeout(timer.handle);
    timers.current.delete(id);
    setItems((list) => list.filter((item) => item.id !== id));
  }, []);

  const schedule = useCallback(
    (id: number) => {
      const timer = timers.current.get(id);
      if (!timer || paused.current) return;
      timer.started = Date.now();
      timer.handle = setTimeout(() => dismiss(id), timer.remaining);
    },
    [dismiss],
  );

  const toast = useCallback(
    (input: ToastInput) => {
      const id = nextId.current++;
      const tone = input.tone ?? "info";
      setItems((list) => [...list, { ...input, id, tone }].slice(-MAX_VISIBLE));
      const base = input.duration === undefined ? DEFAULT_DURATION : input.duration;
      if (base !== null) {
        timers.current.set(id, {
          remaining: input.action ? Math.max(base, ACTION_MIN_DURATION) : base,
          started: 0,
        });
        schedule(id);
      }
      return id;
    },
    [schedule],
  );

  const pause = () => {
    if (paused.current) return;
    paused.current = true;
    for (const timer of timers.current.values()) {
      if (timer.handle) clearTimeout(timer.handle);
      timer.remaining -= Date.now() - timer.started;
    }
  };
  const resume = () => {
    if (!paused.current) return;
    paused.current = false;
    for (const id of timers.current.keys()) schedule(id);
  };

  useEffect(() => {
    const map = timers.current;
    return () => {
      for (const timer of map.values()) if (timer.handle) clearTimeout(timer.handle);
    };
  }, []);

  const api = useMemo(() => ({ toast, dismiss }), [toast, dismiss]);
  const polite = items.filter((i) => i.tone === "info" || i.tone === "success");
  const assertive = items.filter((i) => i.tone === "warning" || i.tone === "danger");

  return (
    <ToastContext.Provider value={api}>
      {children}
      {createPortal(
        <section
          className={styles.region}
          aria-label={t("toast.region")}
          onPointerEnter={pause}
          onPointerLeave={resume}
          onFocus={pause}
          onBlur={(e) => {
            if (!e.currentTarget.contains(e.relatedTarget as Node | null)) resume();
          }}
        >
          <div className={styles.live} role="status" aria-live="polite">
            {polite.map((item) => (
              <ToastView key={item.id} item={item} onDismiss={dismiss} />
            ))}
          </div>
          <div className={styles.live} role="alert" aria-live="assertive">
            {assertive.map((item) => (
              <ToastView key={item.id} item={item} onDismiss={dismiss} />
            ))}
          </div>
        </section>,
        document.body,
      )}
    </ToastContext.Provider>
  );
}

function ToastView({ item, onDismiss }: { item: ToastItem; onDismiss: (id: number) => void }) {
  return (
    <div className={`${styles.toast} ${styles[item.tone]}`}>
      <span className={styles.icon}>
        <Icon name={ICONS[item.tone]} size={18} />
      </span>
      <div className={styles.content}>
        <span className={styles.title}>{item.title}</span>
        {item.description ? <span className={styles.description}>{item.description}</span> : null}
        {item.action ? (
          <div className={styles.actions}>
            <Button
              size="sm"
              onClick={() => {
                item.action?.onClick();
                onDismiss(item.id);
              }}
            >
              {item.action.label}
            </Button>
          </div>
        ) : null}
      </div>
      <IconButton
        icon="close"
        size="sm"
        label={t("toast.dismiss")}
        tooltip={false}
        onClick={() => onDismiss(item.id)}
      />
    </div>
  );
}
