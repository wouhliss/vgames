// Admin web entry. Owner: Agent 3. Functional, animation-free, and defensive: every request, form
// and route handles loading, empty, error, 401/403, 409/412 and offline states.
import "./styles.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./app/App";

async function start(): Promise<void> {
  if (import.meta.env.MODE === "mock") {
    const { startMockWorker } = await import("./mocks/browser");
    await startMockWorker();
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
