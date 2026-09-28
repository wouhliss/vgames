import "@testing-library/jest-dom/vitest";
import { clearMocks } from "@tauri-apps/api/mocks";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

afterEach(async () => {
  cleanup();
  // A component that mounted just before the test ended may still be waiting for `listen` to resolve;
  // `useTauriEvent` then unsubscribes at once, which needs the event mocks. Let it settle first.
  await new Promise((resolve) => setTimeout(resolve, 0));
  clearMocks();
  document.documentElement.removeAttribute("data-modality");
});
