// Inline SVG icons (24×24, stroke-based, currentColor). Decorative by default: pass `label` only
// when the icon is the sole content of something that needs a name (prefer IconButton's `label`).
import type { ReactElement } from "react";

const PATHS = {
  close: "M6 6l12 12M18 6L6 18",
  check: "M5 12.5l4.5 4.5L19 7.5",
  chevronDown: "M6 9l6 6 6-6",
  chevronRight: "M9 6l6 6-6 6",
  chevronLeft: "M15 6l-6 6 6 6",
  arrowLeft: "M19 12H5M11 6l-6 6 6 6",
  more: "M5 12h.01M12 12h.01M19 12h.01",
  info: "M12 11v6M12 7.5h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z",
  warning:
    "M12 9v4M12 16.5h.01M10.3 4.2L2.6 18a2 2 0 001.7 3h15.4a2 2 0 001.7-3L13.7 4.2a2 2 0 00-3.4 0z",
  error: "M12 8v5M12 16h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z",
  search: "M11 18a7 7 0 100-14 7 7 0 000 14zM20 20l-4-4",
  library: "M4 4h4v16H4zM10 4h4v16h-4zM16.5 4.5l3.8 1-4 15.5-3.8-1z",
  browse: "M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z",
  friends:
    "M9 11a4 4 0 100-8 4 4 0 000 8zM2 21v-1a6 6 0 0112 0v1M16 3.5a4 4 0 010 7.5M22 21v-1a6 6 0 00-4-5.6",
  download: "M12 3v12M7 10l5 5 5-5M4 19h16",
  settings:
    "M12 15a3 3 0 100-6 3 3 0 000 6zM19.4 15a1.7 1.7 0 00.3 1.8l.1.1a2 2 0 11-2.8 2.8l-.1-.1a1.7 1.7 0 00-1.8-.3 1.7 1.7 0 00-1 1.5V21a2 2 0 11-4 0v-.1a1.7 1.7 0 00-1.1-1.5 1.7 1.7 0 00-1.8.3l-.1.1a2 2 0 11-2.8-2.8l.1-.1a1.7 1.7 0 00.3-1.8 1.7 1.7 0 00-1.5-1H3a2 2 0 110-4h.1a1.7 1.7 0 001.5-1.1 1.7 1.7 0 00-.3-1.8l-.1-.1a2 2 0 112.8-2.8l.1.1a1.7 1.7 0 001.8.3H9a1.7 1.7 0 001-1.5V3a2 2 0 114 0v.1a1.7 1.7 0 001 1.5 1.7 1.7 0 001.8-.3l.1-.1a2 2 0 112.8 2.8l-.1.1a1.7 1.7 0 00-.3 1.8V9a1.7 1.7 0 001.5 1H21a2 2 0 110 4h-.1a1.7 1.7 0 00-1.5 1z",
  play: "M7 4.5v15l12-7.5z",
  pause: "M7 5h3v14H7zM14 5h3v14h-3z",
  star: "M12 3l2.8 5.7 6.2.9-4.5 4.4 1 6.2L12 17.3 6.5 20.2l1-6.2L3 9.6l6.2-.9z",
  folder: "M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2z",
  copy: "M9 9h11v11H9zM5 15H4V4h11v1",
  external: "M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 01-1 1H5a1 1 0 01-1-1V7a1 1 0 011-1h5",
  refresh: "M20 11a8 8 0 00-14.9-3M4 5v4h4M4 13a8 8 0 0014.9 3M20 19v-4h-4",
  plus: "M12 5v14M5 12h14",
  trash: "M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3",
  user: "M12 12a4 4 0 100-8 4 4 0 000 8zM4 21v-1a7 7 0 0114 0v1",
  server: "M4 4h16v6H4zM4 14h16v6H4zM8 7h.01M8 17h.01",
  shield: "M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z",
  lock: "M6 11h12v10H6zM8 11V7a4 4 0 118 0v4",
  gamepad:
    "M6 8h12a4 4 0 014 4l-.6 4.2a2.5 2.5 0 01-4.4 1.2L15 15H9l-2 2.4a2.5 2.5 0 01-4.4-1.2L2 12a4 4 0 014-4zM7 11v3M5.5 12.5h3M16 12h.01M18 13.5h.01",
  stop: "M6 6h12v12H6z",
  grid: "M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z",
  list: "M8 6h13M8 12h13M8 18h13M3.5 6h.01M3.5 12h.01M3.5 18h.01",
  arrowUp: "M12 19V5M6 11l6-6 6 6",
  arrowDown: "M12 5v14M6 13l6 6 6-6",
  edit: "M4 20h4L19 9l-4-4L4 16zM14 6l4 4",
  cloud: "M7 18h10a4 4 0 00.6-8A6 6 0 006.2 9.2 4.5 4.5 0 007 18z",
  cloudOff: "M3 3l18 18M7 18h10M20.5 14.5A4 4 0 0017.6 10 6 6 0 009 6.5M6.2 9.2A4.5 4.5 0 007 18",
  collection: "M4 6h16M4 12h16M4 18h10",
  chat: "M4 5h16v11H9l-5 4z",
  block: "M12 21a9 9 0 100-18 9 9 0 000 18zM5.6 5.6l12.8 12.8",
  offline:
    "M3 3l18 18M8.5 16.5a5 5 0 017 0M5 13a10 10 0 015.2-2.8M19 13a10 10 0 00-2.4-1.7M2 9.5a15 15 0 014.2-2.6M22 9.5A15 15 0 0010.5 5M12 20h.01",
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({
  name,
  size = 20,
  label,
  filled = false,
}: {
  name: IconName;
  size?: number;
  label?: string;
  filled?: boolean;
}): ReactElement {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill={filled ? "currentColor" : "none"}
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      role={label ? "img" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      focusable="false"
    >
      <path d={PATHS[name]} />
    </svg>
  );
}
