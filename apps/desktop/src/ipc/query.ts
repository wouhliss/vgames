// TanStack Query around the typed commands. The Rust core owns the data and retries network
// calls itself, so queries never refetch on a timer or on focus: they are invalidated by Rust
// events instead (useIpcInvalidation). This keeps the launcher idle when nothing changes.
import { QueryClient, useQueryClient } from "@tanstack/react-query";
import { useTauriEvent } from "./events";
import { events, type FriendList, type Result } from "./index";

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
  downloads: ["downloads_list"] as const,
  updater: ["updater_status"] as const,
  whatsNew: (version: string) => ["updater_whats_new", version] as const,
  socialConnection: ["social_connection"] as const,
  friends: ["friends_list"] as const,
  blocks: ["blocks_list"] as const,
  conversations: ["conversations_list"] as const,
  contactSecurity: (userId: string) => ["contact_security", userId] as const,
  invites: ["invites_list"] as const,
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
  const downloads = () => void client.invalidateQueries({ queryKey: queryKeys.downloads });
  useTauriEvent(events.installsChanged, installs);
  useTauriEvent(events.installFinished, () => {
    installs();
    downloads();
  });
  useTauriEvent(events.downloadsChanged, downloads);
  useTauriEvent(events.gameStarted, installs);
  useTauriEvent(events.gameStopped, installs);
  useTauriEvent(events.collectionsChanged, () => {
    void client.invalidateQueries({ queryKey: queryKeys.collections });
  });

  // Social (05-social-notes §6): the events carry the new state, so most update the cache directly.
  useTauriEvent(events.socialConnectionChanged, (connection) => {
    client.setQueryData(queryKeys.socialConnection, connection);
  });
  useTauriEvent(events.friendsChanged, (list) => {
    client.setQueryData(queryKeys.friends, list);
    void client.invalidateQueries({ queryKey: queryKeys.blocks });
  });
  useTauriEvent(events.presenceChanged, ({ user_id, presence }) => {
    client.setQueryData<FriendList>(queryKeys.friends, (list) =>
      list
        ? {
            ...list,
            friends: list.friends.map((f) => (f.user.id === user_id ? { ...f, presence } : f)),
          }
        : list,
    );
  });
  useTauriEvent(events.conversationsChanged, (conversations) => {
    client.setQueryData(queryKeys.conversations, conversations);
  });
  const invites = () => void client.invalidateQueries({ queryKey: queryKeys.invites });
  useTauriEvent(events.inviteReceived, invites);
  useTauriEvent(events.inviteChanged, invites);
  useTauriEvent(events.deviceNotice, ({ notice }) => {
    void client.invalidateQueries({ queryKey: queryKeys.contactSecurity(notice.user_id) });
  });
}
