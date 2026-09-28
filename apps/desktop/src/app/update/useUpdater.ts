import { useQuery, useQueryClient } from "@tanstack/react-query";
import { commands, events } from "../../ipc";
import { useTauriEvent } from "../../ipc/events";
import { queryKeys } from "../../ipc/query";

/** The updater's status, kept current by the `updater-status` event (no polling). */
export function useUpdaterStatus() {
  const client = useQueryClient();
  useTauriEvent(events.updaterStatus, (status) => {
    client.setQueryData(queryKeys.updater, status);
  });
  return useQuery({ queryKey: queryKeys.updater, queryFn: () => commands.updaterStatus() });
}
