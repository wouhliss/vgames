import "./budget-proof";
// Desktop UI entry. Owner: Agent 3. The UI only talks to the Rust core through the typed IPC layer
// in src/ipc (generated tauri-specta bindings, produced by Agent 2).
import "./styles/global.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./app/App";

async function start(): Promise<void> {
  // `vite --mode mock` runs the UI in a plain browser against in-memory fixtures. The import is
  // behind a compile-time constant, so production bundles contain no mock code.
  if (import.meta.env.MODE === "mock") {
    const { installBrowserMocks } = await import("./mocks/browser");
    installBrowserMocks();
  }
  const root = document.getElementById("root");
  if (!root) return;
  createRoot(root).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

void start();
