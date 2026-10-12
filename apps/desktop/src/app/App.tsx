import { QueryClientProvider } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { RouterProvider } from "react-router/dom";
import { ToastProvider } from "../components/Toast";
import { commands } from "../ipc";
import { createQueryClient, useIpcInvalidation } from "../ipc/query";
import { NavProvider } from "../nav/NavProvider";
import { AppearanceSync } from "./appearance";
import { ErrorBoundary } from "./ErrorBoundary";
import { createAppRouter } from "./router";

function IpcInvalidation(): null {
  useIpcInvalidation();
  return null;
}

/** Shows the committed view without waiting for a hidden WebView to paint. */
function useAppReady(): void {
  useEffect(() => {
    // WebKitGTK suspends animation frames while the native window is hidden.
    // Waiting for one here leaves app_ready waiting on the window it must show.
    commands.appReady().catch(() => {
      // Outside Tauri there is no window to show.
    });
  }, []);
}

export function App() {
  const [queryClient] = useState(createQueryClient);
  const [router] = useState(createAppRouter);
  useAppReady();
  return (
    <ErrorBoundary>
      <QueryClientProvider client={queryClient}>
        <NavProvider>
          <ToastProvider>
            <AppearanceSync />
            <IpcInvalidation />
            <RouterProvider router={router} />
          </ToastProvider>
        </NavProvider>
      </QueryClientProvider>
    </ErrorBoundary>
  );
}
