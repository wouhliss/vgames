// Mounted once in the shell. Turns the core's `save-sync` events into notices (or the conflict
// dialog) and lets any screen open a conflict by id, e.g. when pressing Play returns `save_conflict`.
import { createContext, type ReactNode, useCallback, useContext, useMemo, useState } from "react";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import { events, type SaveSyncEvent } from "../../ipc";
import { useTauriEvent } from "../../ipc/events";
import { ConflictDialog } from "./ConflictDialog";

interface SaveConflictApi {
  openConflict: (conflictId: string) => void;
}

const SaveConflictContext = createContext<SaveConflictApi | null>(null);

/** Outside the shell (unit tests of single screens) opening a conflict does nothing. */
export function useSaveConflicts(): SaveConflictApi {
  return useContext(SaveConflictContext) ?? { openConflict: () => undefined };
}

export function SaveSyncHost({ children }: { children: ReactNode }) {
  const { toast } = useToast();
  const [open, setOpen] = useState<string | null>(null);
  const openConflict = useCallback((id: string) => setOpen(id), []);

  useTauriEvent(events.saveSync, ({ title, outcome }: SaveSyncEvent) => {
    switch (outcome.kind) {
      case "restored":
        toast({
          tone: "info",
          title: t("saves.sync.restored", { title, count: outcome.file_count }),
        });
        return;
      case "uploaded":
        toast({
          tone: "success",
          title: t("saves.sync.uploaded", { title, count: outcome.file_count }),
        });
        return;
      case "pending":
        toast({ tone: "warning", title: t("saves.sync.pending", { title }) });
        return;
      case "conflict":
        // Never resolved for the player: the dialog opens and waits for a choice.
        setOpen(outcome.conflict_id);
        return;
      case "failed":
        toast({
          tone: "danger",
          title: t("saves.sync.failed", { title, detail: outcome.detail }),
        });
        return;
    }
  });

  const api = useMemo(() => ({ openConflict }), [openConflict]);
  return (
    <SaveConflictContext.Provider value={api}>
      {children}
      {open ? <ConflictDialog key={open} conflictId={open} onClose={() => setOpen(null)} /> : null}
    </SaveConflictContext.Provider>
  );
}
