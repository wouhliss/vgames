import { QueryClientProvider } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { RouterProvider } from "react-router/dom";
import { ToastProvider } from "../components/Toast";
import { commands } from "../ipc";
import { createQueryClient, useIpcInvalidation } from "../ipc/query";
import { NavProvider } from "../nav/NavProvider";
import { AppearanceSync } from "./appearance";
import { createAppRouter } from "./router";

function IpcInvalidation(): null {
  useIpcInvalidation();
  return null;
}

/** Shows the window once the first frame is painted (no white flash, A2-T01). */
function useAppReady(): void {
  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      setTimeout(() => {
        commands.appReady().catch(() => {
          // Outside Tauri there is no window to show.
        });
      }, 0);
    });
    return () => cancelAnimationFrame(frame);
  }, []);
}

export function App() {
  const [queryClient] = useState(createQueryClient);
  const [router] = useState(createAppRouter);
  useAppReady();
  return (
    <QueryClientProvider client={queryClient}>
      <NavProvider>
        <ToastProvider>
          <AppearanceSync />
          <IpcInvalidation />
          <RouterProvider router={router} />
        </ToastProvider>
      </NavProvider>
    </QueryClientProvider>
  );
}
