// Button glyphs for the active input device: controller families show their own face buttons
// (by position: south accepts, east goes back), keyboard users see keys. Hidden for pointer users.

import { t } from "../i18n";
import type { ControllerKind, NavAction } from "../ipc";
import { useOptionalNav } from "../nav/NavProvider";
import styles from "./Glyph.module.css";

type GlyphAction = Extract<
  NavAction,
  "accept" | "back" | "menu" | "options" | "tab_prev" | "tab_next"
>;

interface GlyphSpec {
  text: string;
  /** Spoken name, when the visible text is a symbol. */
  name?: string;
  round?: boolean;
}

const XBOX: Record<GlyphAction, GlyphSpec> = {
  accept: { text: "A", round: true },
  back: { text: "B", round: true },
  menu: { text: "Y", round: true },
  options: { text: "☰", name: "Menu" },
  tab_prev: { text: "LB" },
  tab_next: { text: "RB" },
};

const PLAYSTATION: Record<GlyphAction, GlyphSpec> = {
  accept: { text: "✕", name: "Cross", round: true },
  back: { text: "○", name: "Circle", round: true },
  menu: { text: "△", name: "Triangle", round: true },
  options: { text: "OPTIONS" },
  tab_prev: { text: "L1" },
  tab_next: { text: "R1" },
};

// Positional mapping (07-controllers §5): the bottom button accepts, which Nintendo labels B.
const NINTENDO: Record<GlyphAction, GlyphSpec> = {
  accept: { text: "B", round: true },
  back: { text: "A", round: true },
  menu: { text: "X", round: true },
  options: { text: "+" },
  tab_prev: { text: "L" },
  tab_next: { text: "R" },
};

const KEYBOARD: Record<GlyphAction, GlyphSpec> = {
  accept: { text: "Enter" },
  back: { text: "Esc" },
  menu: { text: "Shift+F10" },
  options: { text: "F10" },
  tab_prev: { text: "Ctrl+PgUp" },
  tab_next: { text: "Ctrl+PgDn" },
};

export function glyphFor(controller: ControllerKind | null, action: GlyphAction): GlyphSpec {
  switch (controller) {
    case "dualshock4":
    case "dualsense":
      return PLAYSTATION[action];
    case "switch_pro":
      return NINTENDO[action];
    case "xinput":
    case "generic":
      return XBOX[action];
    case null:
      return KEYBOARD[action];
  }
}

export function Glyph({
  action,
  className,
}: {
  action: GlyphAction;
  className?: string | undefined;
}) {
  const nav = useOptionalNav();
  if (!nav || nav.modality === "pointer") return null;
  const spec = glyphFor(nav.modality === "gamepad" ? (nav.controller ?? "xinput") : null, action);
  return (
    <kbd
      className={[styles.glyph, spec.round && styles.round, className].filter(Boolean).join(" ")}
      title={spec.name}
      aria-label={t("glyph.button", { name: spec.name ?? spec.text })}
    >
      {spec.text}
    </kbd>
  );
}
