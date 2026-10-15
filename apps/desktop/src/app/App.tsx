import { QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";
import { RouterProvider } from "react-router/dom";
import { ToastProvider } from "../components/Toast";
import { createQueryClient, useIpcInvalidation } from "../ipc/query";
import { NavProvider } from "../nav/NavProvider";
import { AppearanceSync } from "./appearance";
import { DeeplinkNotices } from "./DeeplinkNotices";
import { ErrorBoundary } from "./ErrorBoundary";
import { createAppRouter } from "./router";

function IpcInvalidation(): null {
  useIpcInvalidation();
  return null;
}

export function App() {
  const [queryClient] = useState(createQueryClient);
  const [router] = useState(createAppRouter);
  return (
    <ErrorBoundary>
      <QueryClientProvider client={queryClient}>
        <NavProvider>
          <ToastProvider>
            <AppearanceSync />
            <DeeplinkNotices />
            <IpcInvalidation />
            <RouterProvider router={router} />
          </ToastProvider>
        </NavProvider>
      </QueryClientProvider>
    </ErrorBoundary>
  );
}
