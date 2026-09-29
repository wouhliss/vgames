import { QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";
import { RouterProvider } from "react-router/dom";
import { setUnauthorizedHandler } from "../api/http";
import { createQueryClient } from "../api/query";
import { connectivity } from "./connectivity";
import { createAppRouter } from "./router";
import { currentReturnTo, leaveForSignIn } from "./session";

export function App() {
  const [router] = useState(createAppRouter);
  const [queryClient] = useState(() => {
    const qc = createQueryClient({ onNetworkError: connectivity.markOffline });
    setUnauthorizedHandler(() =>
      leaveForSignIn(
        qc,
        (to) => void router.navigate(to),
        currentReturnTo(),
        window.location.pathname.startsWith("/admin/login"),
      ),
    );
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
