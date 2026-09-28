// Privacy and Overlay both edit Agent 4's SocialSettings (generated `social_settings_get|set`): one
// query, one save that reports typed errors (the overlay shortcut can be refused by the Rust core).
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback } from "react";
import { commands, type SocialSettings, type SocialSettingsError } from "../../ipc";
import { unwrap } from "../../ipc/query";

export const SOCIAL_SETTINGS = ["social_settings_get"] as const;

export function useSocialSettings() {
  const client = useQueryClient();
  const query = useQuery({
    queryKey: SOCIAL_SETTINGS,
    queryFn: async () => unwrap(await commands.socialSettingsGet()),
  });
  const save = useCallback(
    async (next: SocialSettings): Promise<SocialSettingsError | "failed" | null> => {
      const result = await commands.socialSettingsSet(next).catch(() => null);
      if (!result) return "failed";
      if (result.status === "error") return result.error;
      client.setQueryData(SOCIAL_SETTINGS, result.data);
      return null;
    },
    [client],
  );
  return { query, save };
}
