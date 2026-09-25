import { QueryClientProvider } from "@tanstack/react-query";
import { type RenderOptions, render } from "@testing-library/react";
import type { ReactElement, ReactNode } from "react";
import { ToastProvider } from "../components/Toast";
import { createQueryClient, useIpcInvalidation } from "../ipc/query";
import { NavProvider } from "../nav/NavProvider";

function IpcInvalidation(): null {
  useIpcInvalidation();
  return null;
}

/** The same providers as `App`, including Rust-event-driven query invalidation. */
export function Providers({ children }: { children: ReactNode }) {
  return (
    <QueryClientProvider client={createQueryClient()}>
      <NavProvider>
        <ToastProvider>
          <IpcInvalidation />
          {children}
        </ToastProvider>
      </NavProvider>
    </QueryClientProvider>
  );
}

export function renderWithProviders(ui: ReactElement, options?: RenderOptions) {
  return render(ui, { wrapper: Providers, ...options });
}

/** jsdom has no layout: give elements explicit boxes for spatial navigation tests. */
export function setRect(
  el: Element,
  left: number,
  top: number,
  width: number,
  height: number,
): void {
  el.getBoundingClientRect = () =>
    ({
      left,
      top,
      right: left + width,
      bottom: top + height,
      width,
      height,
      x: left,
      y: top,
      toJSON: () => ({}),
    }) as DOMRect;
}

/** Lets pending listen()/invoke() promises settle. */
export async function flush(): Promise<void> {
  for (let i = 0; i < 5; i += 1) await Promise.resolve();
  await new Promise((r) => setTimeout(r, 0));
}
