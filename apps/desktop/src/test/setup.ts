import "@testing-library/jest-dom/vitest";
import { clearMocks } from "@tauri-apps/api/mocks";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";
import { cancelScheduled } from "../mocks/runtime";

afterEach(async () => {
  cleanup();
  // A component that mounted just before the test ended may still be waiting for `listen` to resolve;
  // `useTauriEvent` then unsubscribes at once, which needs the event mocks. Let it settle first.
  await new Promise((resolve) => setTimeout(resolve, 0));
  // Simulated work still pending (an install finishing, a message being sent) must not emit
  // through the IPC mocks once they are gone.
  cancelScheduled();
  clearMocks();
  document.documentElement.removeAttribute("data-modality");
});
