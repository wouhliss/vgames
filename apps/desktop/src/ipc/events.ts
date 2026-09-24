import type { EventCallback, UnlistenFn } from "@tauri-apps/api/event";
import { useEffect, useRef } from "react";

type Listenable<T> = { listen: (cb: EventCallback<T>) => Promise<UnlistenFn> };

/**
 * Subscribes to a Rust event for the lifetime of the component. The handler can change on every
 * render without resubscribing. Handles the race where the component unmounts before `listen`
 * resolves, so navigation never leaks listeners.
 */
export function useTauriEvent<T>(event: Listenable<T>, handler: (payload: T) => void): void {
  const handlerRef = useRef(handler);
  handlerRef.current = handler;
  useEffect(() => {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    event
      .listen((e) => {
        if (!disposed) handlerRef.current(e.payload);
      })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch(() => {
        // Outside Tauri (unit tests without event mocks) there is nothing to listen to.
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [event]);
}
