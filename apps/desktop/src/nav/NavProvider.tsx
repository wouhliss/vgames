// Keyboard and gamepad navigation for the whole launcher.
//
// Gamepad input arrives from Rust as `ui-nav` events (the controllers module debounces sticks and
// auto-repeats). Each intent is replayed as the equivalent key on the focused element, so composite
// widgets (tabs, menus, listboxes) handle the D-pad exactly as they handle arrow keys. Keys nobody
// handled fall through to this provider:
//
//   arrows / D-pad / stick  → move focus geometrically inside the top focus scope
//   Enter / A               → activate (buttons react natively; for the pad we click())
//   Escape / B              → close the top layer (dialogs, menus handle Escape) or go back
//   Ctrl+PageUp/Down / LB/RB → previous / next tab of the innermost registered tab set
//   Shift+F10, ContextMenu / Y → the focused element's context menu
//
// The last input modality is written to <html data-modality> so focus rings are always visible
// for keyboard and gamepad users (global.css).
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
import { type ControllerKind, events, type NavAction } from "../ipc";
import { useTauriEvent } from "../ipc/events";
import { focusElement, isTextEntry, tabbable } from "./focus";
import { type Direction, pickNext, type Rect } from "./geometry";

export type Modality = "pointer" | "keyboard" | "gamepad";

type TabSwitcher = (dir: -1 | 1) => void;
type BackHandler = () => boolean;

interface NavContextValue {
  modality: Modality;
  controller: ControllerKind | null;
  /** Restricts spatial movement to `el` (dialogs, open menus). Returns the release function. */
  pushScope: (el: HTMLElement) => () => void;
  /** Registers a tab set for LB/RB. The innermost (last registered, still connected) one wins. */
  registerTabs: (el: HTMLElement, switcher: TabSwitcher) => () => void;
  /** Registers a handler for Escape/B when no layer consumed it. Return true when handled. */
  registerBack: (handler: BackHandler) => () => void;
}

const NavContext = createContext<NavContextValue | null>(null);

export function useNav(): NavContextValue {
  const ctx = useContext(NavContext);
  if (!ctx) throw new Error("useNav must be used inside <NavProvider>");
  return ctx;
}

/** Optional variant for components that also render outside the provider (tests, overlays). */
export function useOptionalNav(): NavContextValue | null {
  return useContext(NavContext);
}

const KEY_FOR_DIRECTION: Record<Direction, string> = {
  up: "ArrowUp",
  down: "ArrowDown",
  left: "ArrowLeft",
  right: "ArrowRight",
};
const DIRECTION_FOR_KEY: Record<string, Direction> = {
  ArrowUp: "up",
  ArrowDown: "down",
  ArrowLeft: "left",
  ArrowRight: "right",
};

/** Synthetic key events created for gamepad intents (so handlers can tell them apart). */
const fromGamepad = new WeakSet<Event>();

export function isGamepadEvent(event: Event): boolean {
  return fromGamepad.has(event);
}

function rectOf(el: Element): Rect {
  const r = el.getBoundingClientRect();
  return { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
}

// Last focused element per navigation group ([data-nav-group]), so moving back into a sidebar or a
// grid lands where the user left it rather than on the geometrically nearest item.
const groupMemory = new WeakMap<Element, HTMLElement>();

function rememberFocus(el: HTMLElement): void {
  let group = el.parentElement?.closest("[data-nav-group]");
  while (group) {
    groupMemory.set(group, el);
    group = group.parentElement?.closest("[data-nav-group]") ?? null;
  }
}

function isUsable(el: HTMLElement, scope: HTMLElement): boolean {
  if (!el.isConnected || !scope.contains(el)) return false;
  const r = el.getBoundingClientRect();
  return r.width > 0 || r.height > 0;
}

export function moveFocus(scope: HTMLElement, dir: Direction): boolean {
  const current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const candidates = tabbable(scope).filter((el) => el !== current);
  if (candidates.length === 0) return false;
  if (!current || current === document.body || !scope.contains(current)) {
    const first = candidates.find((el) => isUsable(el, scope));
    if (first) focusElement(first);
    return first !== undefined;
  }
  const index = pickNext(rectOf(current), candidates.map(rectOf), dir);
  const target = candidates[index];
  if (!target) return false;
  const group = target.closest("[data-nav-group]");
  if (group && !group.contains(current)) {
    const remembered = groupMemory.get(group);
    if (remembered && isUsable(remembered, scope) && remembered.tabIndex >= 0) {
      focusElement(remembered);
      return true;
    }
  }
  focusElement(target);
  return true;
}

/** Arrow keys inside a text field move the caret, except at the edges (and never for the pad). */
function textEntryKeepsKey(el: Element | null, key: string, synthetic: boolean): boolean {
  if (synthetic || !isTextEntry(el)) return false;
  if (el instanceof HTMLTextAreaElement) return true;
  const start = el.selectionStart;
  const end = el.selectionEnd;
  if (start === null || end === null) return false;
  if (key === "ArrowLeft") return !(start === 0 && end === 0);
  if (key === "ArrowRight") return !(start === el.value.length && end === el.value.length);
  return false;
}

function dispatchKey(
  target: EventTarget,
  key: string,
  init: KeyboardEventInit = {},
): KeyboardEvent {
  const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init });
  fromGamepad.add(event);
  target.dispatchEvent(event);
  return event;
}

function setModalityAttr(m: Modality): void {
  document.documentElement.dataset.modality = m;
}

export function NavProvider({ children }: { children: ReactNode }) {
  const [modality, setModality] = useState<Modality>("pointer");
  const [controller, setController] = useState<ControllerKind | null>(null);
  const scopes = useRef<HTMLElement[]>([]);
  const tabSets = useRef<{ el: HTMLElement; switcher: TabSwitcher }[]>([]);
  const backHandlers = useRef<BackHandler[]>([]);
  const modalityRef = useRef<Modality>("pointer");

  const updateModality = useCallback((m: Modality) => {
    if (modalityRef.current === m) return;
    modalityRef.current = m;
    setModalityAttr(m);
    setModality(m);
  }, []);

  const topScope = useCallback((): HTMLElement => {
    const live = scopes.current.filter((el) => el.isConnected);
    scopes.current = live;
    return live[live.length - 1] ?? document.body;
  }, []);

  const switchTab = useCallback(
    (dir: -1 | 1) => {
      const scope = topScope();
      const live = tabSets.current.filter((t) => t.el.isConnected && scope.contains(t.el));
      const active = document.activeElement;
      // Prefer a tab set containing focus, else the innermost one registered.
      const containing = live.filter((t) => active && t.el.parentElement?.contains(active));
      const target = containing[containing.length - 1] ?? live[live.length - 1];
      target?.switcher(dir);
    },
    [topScope],
  );

  const goBack = useCallback(() => {
    for (let i = backHandlers.current.length - 1; i >= 0; i -= 1) {
      if (backHandlers.current[i]?.()) return;
    }
  }, []);

  // Keys nobody handled: spatial movement, tab switching, back.
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      const synthetic = fromGamepad.has(e);
      if (!synthetic) {
        if (e.key === "Tab" || e.key in DIRECTION_FOR_KEY) updateModality("keyboard");
      }
      if (e.defaultPrevented) return;
      const dir = DIRECTION_FOR_KEY[e.key];
      if (dir && !e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey) {
        if (textEntryKeepsKey(document.activeElement, e.key, synthetic)) return;
        if (moveFocus(topScope(), dir)) e.preventDefault();
        return;
      }
      if (e.ctrlKey && (e.key === "PageUp" || e.key === "PageDown")) {
        e.preventDefault();
        switchTab(e.key === "PageUp" ? -1 : 1);
        return;
      }
      if (e.key === "Escape") {
        goBack();
      }
    }
    function onPointerDown() {
      updateModality("pointer");
    }
    function onFocusIn(e: FocusEvent) {
      if (e.target instanceof HTMLElement) rememberFocus(e.target);
    }
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("focusin", onFocusIn);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("focusin", onFocusIn);
    };
  }, [goBack, switchTab, topScope, updateModality]);

  const handleNav = useCallback(
    (action: NavAction) => {
      updateModality("gamepad");
      const active =
        document.activeElement instanceof HTMLElement ? document.activeElement : document.body;
      switch (action) {
        case "up":
        case "down":
        case "left":
        case "right":
          dispatchKey(active, KEY_FOR_DIRECTION[action]);
          return;
        case "accept": {
          if (active === document.body || isTextEntry(active)) return;
          const e = dispatchKey(active, "Enter");
          if (!e.defaultPrevented) active.click();
          return;
        }
        case "back":
          dispatchKey(active, "Escape");
          return;
        case "menu":
          dispatchKey(active, "ContextMenu");
          return;
        case "options":
          active.dispatchEvent(new CustomEvent("vg:options", { bubbles: true }));
          return;
        case "tab_prev":
          switchTab(-1);
          return;
        case "tab_next":
          switchTab(1);
          return;
      }
    },
    [switchTab, updateModality],
  );

  useTauriEvent(events.uiNav, (payload) => {
    setController(payload.controller);
    handleNav(payload.action);
  });
  useTauriEvent(events.activeControllerChanged, (payload) => setController(payload.controller));

  const pushScope = useCallback((el: HTMLElement) => {
    scopes.current.push(el);
    return () => {
      scopes.current = scopes.current.filter((s) => s !== el);
    };
  }, []);

  const registerTabs = useCallback((el: HTMLElement, switcher: TabSwitcher) => {
    const entry = { el, switcher };
    tabSets.current.push(entry);
    return () => {
      tabSets.current = tabSets.current.filter((t) => t !== entry);
    };
  }, []);

  const registerBack = useCallback((handler: BackHandler) => {
    backHandlers.current.push(handler);
    return () => {
      backHandlers.current = backHandlers.current.filter((h) => h !== handler);
    };
  }, []);

  const value = useMemo<NavContextValue>(
    () => ({ modality, controller, pushScope, registerTabs, registerBack }),
    [modality, controller, pushScope, registerTabs, registerBack],
  );

  return <NavContext.Provider value={value}>{children}</NavContext.Provider>;
}

/** Runs `handler` on Escape / B when no dialog or menu consumed it (e.g. "back" on a sub-page). */
export function useBackHandler(handler: BackHandler | null): void {
  const registerBack = useOptionalNav()?.registerBack;
  const ref = useRef(handler);
  ref.current = handler;
  const enabled = handler !== null;
  useEffect(() => {
    if (!registerBack || !enabled) return;
    return registerBack(() => ref.current?.() ?? false);
  }, [registerBack, enabled]);
}
