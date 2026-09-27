// Turning a key press into an accelerator string ("Ctrl+Shift+O", "Shift+F3") for the overlay
// shortcut. Keys are named by position (KeyboardEvent.code), so the shortcut doesn't change with the
// keyboard layout. The Rust core registers it and reports conflicts; this only refuses combinations
// that could never work as a global shortcut.

export const DEFAULT_HOTKEY = "Shift+F3";

export type HotkeyResult =
  | { kind: "ok"; accelerator: string }
  /** Only modifiers so far: keep listening. */
  | { kind: "pending" }
  /** A plain key (or Shift + key) would fire while typing. */
  | { kind: "needs_modifier" }
  /** A key vgames doesn't name (e.g. media keys, IME composition). */
  | { kind: "unsupported" };

const NAMED: Record<string, string> = {
  Space: "Space",
  Tab: "Tab",
  Enter: "Enter",
  Backspace: "Backspace",
  Delete: "Delete",
  Insert: "Insert",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  Backquote: "`",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
};

const MODIFIER_CODES = new Set([
  "ShiftLeft",
  "ShiftRight",
  "ControlLeft",
  "ControlRight",
  "AltLeft",
  "AltRight",
  "MetaLeft",
  "MetaRight",
]);

function keyName(code: string): string | null {
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter?.[1]) return letter[1];
  const digit = /^Digit(\d)$/.exec(code);
  if (digit?.[1]) return digit[1];
  const numpad = /^Numpad(\d)$/.exec(code);
  if (numpad?.[1]) return `num${numpad[1]}`;
  if (/^F([1-9]|1\d|2[0-4])$/.test(code)) return code;
  return NAMED[code] ?? null;
}

type KeyLike = Pick<KeyboardEvent, "code" | "ctrlKey" | "altKey" | "shiftKey" | "metaKey">;

export function acceleratorFrom(e: KeyLike): HotkeyResult {
  if (MODIFIER_CODES.has(e.code)) return { kind: "pending" };
  const key = keyName(e.code);
  if (!key) return { kind: "unsupported" };
  const isFunctionKey = /^F\d+$/.test(key);
  if (!e.ctrlKey && !e.altKey && !e.metaKey && !isFunctionKey) return { kind: "needs_modifier" };
  const parts = [
    e.ctrlKey ? "Ctrl" : null,
    e.altKey ? "Alt" : null,
    e.shiftKey ? "Shift" : null,
    e.metaKey ? "Super" : null,
    key,
  ].filter(Boolean);
  return { kind: "ok", accelerator: parts.join("+") };
}
