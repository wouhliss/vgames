// Admin web entry. Owner: Agent 3. Functional, animation-free, and defensive:
// every request, form and route must handle loading, empty, error, 401/403,
// 409 and offline states (docs/agents/agent-3-frontend.md).
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <p>vgames admin</p>
    </StrictMode>,
  );
}
