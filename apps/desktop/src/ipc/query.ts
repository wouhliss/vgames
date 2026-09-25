// TanStack Query around the typed commands. The Rust core owns the data and retries network
// calls itself, so queries never refetch on a timer or on focus: they are invalidated by Rust
// events instead (useIpcInvalidation). This keeps the launcher idle when nothing changes.
import { QueryClient, useQueryClient } from "@tanstack/react-query";
import { useTauriEvent } from "./events";
import { events, type Result } from "./index";

/** A command returned its typed error. `error` is the Rust enum value. */
export class CommandError<E> extends Error {
  readonly error: E;
  constructor(error: E) {
    super(
      typeof error === "object" && error !== null && "kind" in error
        ? String(error.kind)
        : "command_error",
    );
    this.name = "CommandError";
    this.error = error;
  }
}

export function unwrap<T, E>(result: Result<T, E>): T {
  if (result.status === "ok") return result.data;
  throw new CommandError(result.error);
}

export function commandError<E>(error: unknown): E | null {
  return error instanceof CommandError ? (error.error as E) : null;
}

/** One key per command (plus its arguments). Invalidation targets these prefixes. */
export const queryKeys = {
  appInfo: ["app_info"] as const,
  appearance: ["appearance_get"] as const,
  servers: ["servers_list"] as const,
  libraries: ["libraries_list"] as const,
  installs: ["installs_list"] as const,
  collections: ["collections_list"] as const,
  catalog: ["catalog_list"] as const,
  genres: ["catalog_genres"] as const,
  details: (packageId: string) => ["package_details", packageId] as const,
};

export function createQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: {
        staleTime: Number.POSITIVE_INFINITY,
        gcTime: 5 * 60_000,
        retry: false,
        refetchOnWindowFocus: false,
        refetchOnReconnect: false,
        // IPC does not depend on the browser's idea of being online.
        networkMode: "always",
      },
      mutations: { retry: false, networkMode: "always" },
    },
  });
}

/** Maps "something changed" events from Rust to query invalidations. Mounted once, in the app root. */
export function useIpcInvalidation(): void {
  const client = useQueryClient();
  useTauriEvent(events.serversChanged, () => {
    void client.invalidateQueries({ queryKey: queryKeys.servers });
  });
  useTauriEvent(events.librariesChanged, () => {
    void client.invalidateQueries({ queryKey: queryKeys.libraries });
  });
  const installs = () => void client.invalidateQueries({ queryKey: queryKeys.installs });
  useTauriEvent(events.installsChanged, installs);
  useTauriEvent(events.installFinished, installs);
  useTauriEvent(events.gameStarted, installs);
  useTauriEvent(events.gameStopped, installs);
  useTauriEvent(events.collectionsChanged, () => {
    void client.invalidateQueries({ queryKey: queryKeys.collections });
  });
}
