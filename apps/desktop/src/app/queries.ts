import { useQuery } from "@tanstack/react-query";
import { commands } from "../ipc";
import { queryKeys, unwrap } from "../ipc/query";

export function useServers() {
  return useQuery({ queryKey: queryKeys.servers, queryFn: () => commands.serversList() });
}

export function useActiveServer() {
  const servers = useServers();
  return { ...servers, server: servers.data?.find((s) => s.active) ?? null };
}

export function useLibraries() {
  return useQuery({
    queryKey: queryKeys.libraries,
    queryFn: async () => unwrap(await commands.librariesList()),
  });
}

export function useAppInfo() {
  return useQuery({ queryKey: queryKeys.appInfo, queryFn: () => commands.appInfo() });
}

/** What the first-run flow still needs, or null when the app is ready to use. */
export type OnboardingNeed = "server" | "sign_in" | "library" | null;

export function useOnboardingNeed(): { need: OnboardingNeed; loading: boolean; error: unknown } {
  const servers = useActiveServer();
  const libraries = useLibraries();
  if (servers.isPending || libraries.isPending) return { need: null, loading: true, error: null };
  if (servers.error || libraries.error)
    return { need: null, loading: false, error: servers.error ?? libraries.error };
  if (!servers.server) return { need: "server", loading: false, error: null };
  if (!servers.server.account) return { need: "sign_in", loading: false, error: null };
  if ((libraries.data ?? []).length === 0) return { need: "library", loading: false, error: null };
  return { need: null, loading: false, error: null };
}
