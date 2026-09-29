// Overlay window entry (label `overlay`, 05-social-notes §7). Owner: Agent 4.
// The macOS panel and the always-on-top fallback window show this page above the game. It may
// only call `overlay_view` / `overlay_action` and listen to `overlay-view`.
import "./overlay.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { OverlayApp } from "./OverlayApp";

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <OverlayApp />
    </StrictMode>,
  );
}
