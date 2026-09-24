// Desktop UI entry. Owner: Agent 3. The UI only talks to the Rust core through
// the generated tauri-specta bindings in src/bindings.ts (produced by Agent 2).
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <p>vgames</p>
    </StrictMode>,
  );
}
