import { QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";
import { RouterProvider } from "react-router/dom";
import { createQueryClient } from "../api/query";
import { connectivity } from "./connectivity";
import { createAppRouter } from "./router";
import { currentReturnTo, meKey } from "./session";

export function App() {
  const [router] = useState(createAppRouter);
  const [queryClient] = useState(() => {
    const qc = createQueryClient({
      onUnauthenticated: () => {
        // The session ended mid-use: drop cached identity and go to sign-in, coming back here after.
        qc.removeQueries({ queryKey: meKey });
        if (!window.location.pathname.startsWith("/admin/login")) {
          void router.navigate(`/login?return_to=${encodeURIComponent(currentReturnTo())}`);
        }
      },
      onNetworkError: connectivity.markOffline,
    });
    qc.getQueryCache().subscribe((event) => {
      if (event.type === "updated" && event.action.type === "success") connectivity.markOnline();
    });
    return qc;
  });
  return (
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  );
}
